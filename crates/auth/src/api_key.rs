//! API key service (#4.6).
//!
//! Per-user programmatic-access keys. The cleartext key is
//! `tdh_<base64url(32 bytes of OsRng)>` — a 4-char human-readable
//! prefix plus 43 chars of URL-safe-base64 entropy. The cleartext
//! is shown ONCE in the creation response and the DB only keeps:
//!
//!   * `key_hash`  = SHA-256 of the cleartext (lookup column)
//!   * `key_prefix` = `tdh_<first KEY_PREFIX_VISIBLE_CHARS of
//!     entropy>` (display-only identifier, never the secret)
//!
//! `verify`'s comparison happens server-side via the DB equality
//! predicate `WHERE key_hash = $1` on the SHA-256 hash, NOT via
//! a Rust constant-time compare — the UNIQUE index on
//! `mcp_api_keys.key_hash` makes the lookup a single btree
//! probe and the surrounding request latency dwarfs any
//! per-byte timing signal.
//!
//! Pre-validation via [`is_well_shaped`] short-circuits inputs
//! that don't match the documented cleartext format BEFORE
//! hashing or any DB round trip: this rejects obviously-bogus
//! values (truncated cookies, JWTs, random URLs) at near-zero
//! cost. Well-shaped inputs ARE hashed and looked up — they
//! have to be, since "well-shaped but unissued" is the
//! attacker's main attack vector and the DB miss is the only
//! way to discriminate it from a real key. So the precise
//! guarantee is: pre-validation gates `Sha256::digest` to
//! exactly the bytes that match the public cleartext format,
//! not "no attacker bytes ever". The trade-off is explicit:
//! timing leaks the well-shaped / malformed distinction
//! (microseconds for the in-process reject vs. milliseconds
//! for the DB miss), which is acceptable because the cleartext
//! format is public knowledge (`tdh_` prefix + base64url
//! alphabet + 43 chars) and shape doesn't narrow the search
//! space.

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use rand::TryRngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use storage::{ApiKeyRepo, ApiKeyRow, NewApiKey, StorageError};
use uuid::Uuid;

use crate::error::AuthError;

/// Cleartext key prefix. Always emitted on minted keys so users
/// can tell at a glance that a string belongs to this product
/// (the same trick `Stripe`, `GitHub`, `OpenAI` etc. use).
pub const API_KEY_HUMAN_PREFIX: &str = "tdh_";

/// Entropy bytes that go into the base64url body of the key.
/// 32 bytes = 256 bits — same envelope as the session token.
const API_KEY_ENTROPY_BYTES: usize = 32;

/// Cleartext base64url body length for [`API_KEY_ENTROPY_BYTES`]
/// bytes of input. URL-safe-no-pad encoding of 32 bytes is 43
/// characters; we hard-code the constant so the pre-validation
/// length check on `verify` is a single integer compare instead
/// of running base64 decode on rubbish.
const API_KEY_BODY_LEN: usize = 43;

/// How many characters of the body to embed in `key_prefix`. The
/// UI shows `tdh_a1b2…` so the user can identify which row in
/// their list is which without ever re-displaying the secret.
/// 8 chars of base64url ≈ 48 bits — enough to disambiguate
/// realistic key counts per user without leaking material that
/// shortens a brute-force search meaningfully (the remaining 35
/// chars hold ~208 bits of entropy).
const API_KEY_PREFIX_VISIBLE_CHARS: usize = 8;

/// Maximum retry attempts on a PK / unique-violation collision.
/// `OsRng` over 32 bytes makes a real collision astronomical
/// (~2^-256); hitting the retry path twice in a row implies a
/// defective RNG — surface the error rather than spin.
const ISSUE_MAX_ATTEMPTS: u32 = 3;

/// Allowed values for `mcp_api_keys.rate_limit_tier`. Mirrors
/// the migration's CHECK constraint so the validation surfaces
/// here as a typed error before the DB rejects the insert.
pub const ALLOWED_TIERS: &[&str] = &["free", "pro", "enterprise"];

/// Total length of a cleartext key (`tdh_` + 43 base64url chars).
const API_KEY_TOTAL_LEN: usize = API_KEY_HUMAN_PREFIX.len() + API_KEY_BODY_LEN;

/// Default tier assigned when the caller doesn't specify one.
/// Matches the table-level `DEFAULT 'free'`.
pub const DEFAULT_RATE_LIMIT_TIER: &str = "free";

/// What [`ApiKeyService::issue`] returns to the gateway on
/// success. The cleartext key is part of THIS shape and ONLY
/// this shape — the gateway forwards it to the HTTP response
/// once and then drops it.
#[derive(Debug, Clone)]
pub struct IssuedApiKey {
    /// Persisted row id (`uuidv7`).
    pub id: Uuid,
    /// Cleartext `tdh_<…>` key. SHOWN ONCE; never persisted.
    pub cleartext: String,
    /// Public identifier (`tdh_a1b2…`). Same value the DB
    /// stores in `key_prefix`; the gateway can echo this back in
    /// later list responses.
    pub key_prefix: String,
}

/// What [`ApiKeyService::verify`] returns on a successful lookup.
/// The cleartext is intentionally absent — once verified, the
/// downstream rate-limit and authorisation paths only need the
/// row metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedApiKey {
    pub id: Uuid,
    pub user_id: Uuid,
    pub scopes: Vec<String>,
    pub rate_limit_tier: String,
}

/// Compose root for the API-key surface. The gateway builds one
/// `Arc<ApiKeyService>` at startup and reuses it across handlers.
pub struct ApiKeyService {
    keys: Arc<dyn ApiKeyRepo>,
}

impl ApiKeyService {
    #[must_use]
    pub fn new(keys: Arc<dyn ApiKeyRepo>) -> Self {
        Self { keys }
    }

    /// Mint a fresh API key for `user_id`.
    ///
    /// Generates the cleartext, hashes it, inserts the row, and
    /// returns `(id, cleartext, key_prefix)`. The caller is
    /// responsible for SHOWING the cleartext to the user ONCE
    /// and not storing it anywhere afterwards.
    ///
    /// Retries up to [`ISSUE_MAX_ATTEMPTS`] times on
    /// [`StorageError::UniqueViolation`] (cosmic-ray hash
    /// collision). Beyond that, surfaces `Internal` rather than
    /// looping forever.
    pub async fn issue(
        &self,
        user_id: Uuid,
        name: String,
        scopes: Vec<String>,
        rate_limit_tier: String,
    ) -> Result<IssuedApiKey, AuthError> {
        // Defensive tier validation before any DB work — the
        // gateway should have validated this at the HTTP layer
        // already, but mirroring the CHECK here gives a clean
        // typed error instead of a Postgres constraint surface.
        if !ALLOWED_TIERS.contains(&rate_limit_tier.as_str()) {
            return Err(AuthError::Validation(format!(
                "rate_limit_tier `{rate_limit_tier}` is not one of {ALLOWED_TIERS:?}",
            )));
        }
        if name.trim().is_empty() {
            return Err(AuthError::Validation(
                "api key name must not be empty".into(),
            ));
        }

        // Normalise scopes IN THE SERVICE (not just at the
        // SvelteKit form layer) so every caller — current web
        // form, future MCP / CLI clients, batch importers —
        // gets the same row shape: trim each entry, drop
        // empties, sort + dedup. Without this, callers can
        // persist `["", "  ", "read"]` and the "empty scopes
        // means no elevated capabilities" invariant fails to
        // hold downstream.
        let scopes = normalise_scopes(scopes);

        for attempt in 1..=ISSUE_MAX_ATTEMPTS {
            match self
                .try_issue_once(user_id, &name, &scopes, &rate_limit_tier)
                .await
            {
                Ok(issued) => return Ok(issued),
                Err(AuthError::Storage(StorageError::UniqueViolation(_)))
                    if attempt < ISSUE_MAX_ATTEMPTS =>
                {
                    // Cosmic-ray collision — fall through to the
                    // next iteration with fresh entropy. The
                    // attempts log is intentionally sparse; the
                    // next round writes a different hash so the
                    // duplicate row won't reappear.
                }
                Err(AuthError::Storage(StorageError::UniqueViolation(_))) => {
                    return Err(AuthError::Internal(format!(
                        "api key insert collided {ISSUE_MAX_ATTEMPTS} times in a row — RNG suspect"
                    )));
                }
                Err(other) => return Err(other),
            }
        }
        // Loop body either returns Ok or surfaces an error; the
        // unreachable arm exists only to make the compiler
        // happy about the `for` exit path.
        Err(AuthError::Internal(
            "api key issue exhausted retries without surfacing an error".into(),
        ))
    }

    async fn try_issue_once(
        &self,
        user_id: Uuid,
        name: &str,
        scopes: &[String],
        rate_limit_tier: &str,
    ) -> Result<IssuedApiKey, AuthError> {
        let mut entropy = [0u8; API_KEY_ENTROPY_BYTES];
        OsRng
            .try_fill_bytes(&mut entropy)
            .expect("OsRng must provide entropy for api key minting");
        let body = URL_SAFE_NO_PAD.encode(entropy);
        // base64url-no-pad of 32 bytes = 43 chars; the constant
        // mirrors that so any change in entropy size is caught
        // at the assert.
        debug_assert_eq!(body.len(), API_KEY_BODY_LEN);
        let cleartext = format!("{API_KEY_HUMAN_PREFIX}{body}");
        let key_hash = Sha256::digest(cleartext.as_bytes()).to_vec();
        let key_prefix = format!(
            "{API_KEY_HUMAN_PREFIX}{}",
            &body[..API_KEY_PREFIX_VISIBLE_CHARS]
        );

        // Single wall-clock source for the whole row, mirroring
        // the #4.5 session-row pattern. `created_at` is the
        // anchor — every subsequent `last_used_at` and
        // `revoked_at` update reads `Utc::now()` again, so
        // sharing the clock here keeps the audit timeline
        // `created_at <= last_used_at <= revoked_at` true even
        // under app/DB clock skew.
        let now = Utc::now();
        let id = self
            .keys
            .insert_api_key(NewApiKey {
                user_id,
                name: name.to_owned(),
                key_prefix: key_prefix.clone(),
                key_hash,
                scopes: scopes.to_vec(),
                rate_limit_tier: rate_limit_tier.to_owned(),
                created_at: now,
            })
            .await
            .map_err(AuthError::Storage)?;
        Ok(IssuedApiKey {
            id,
            cleartext,
            key_prefix,
        })
    }

    /// Verify an inbound API key. Returns:
    ///
    /// - `Ok(Some(VerifiedApiKey))` for a live, unrevoked key
    ///   whose `tdh_<43-char-body>` shape and SHA-256 both match
    ///   a row. Side effect: `last_used_at` is touched to `now`.
    /// - `Ok(None)` for missing / revoked / malformed key.
    ///
    /// Malformed values do NOT surface as errors — the caller
    /// treats "no key" and "bad key" identically (anonymous
    /// request). [`is_well_shaped`] short-circuits malformed
    /// input before any DB round trip; a well-shaped but
    /// unknown key still pays one btree probe through the
    /// UNIQUE index on `mcp_api_keys.key_hash`. The revocation
    /// filter (`revoked_at IS NULL`) is in the
    /// `touch_and_verify` `UPDATE … RETURNING` predicate, not
    /// the index itself — the index is unique across all rows
    /// so a revoked key's hash can never be re-issued. The
    /// timing difference between the two reject paths leaks
    /// the well-shaped / malformed distinction, which is
    /// acceptable because the cleartext format is publicly
    /// documented (see module-level docs).
    pub async fn verify(&self, cleartext: &str) -> Result<Option<VerifiedApiKey>, AuthError> {
        if !is_well_shaped(cleartext) {
            return Ok(None);
        }
        let key_hash = Sha256::digest(cleartext.as_bytes()).to_vec();
        let row = self
            .keys
            .touch_and_verify(&key_hash, Utc::now())
            .await
            .map_err(AuthError::Storage)?;
        Ok(row.map(|r| VerifiedApiKey {
            id: r.id,
            user_id: r.user_id,
            scopes: r.scopes,
            rate_limit_tier: r.rate_limit_tier,
        }))
    }

    /// List all keys belonging to `user_id`, ordered most-recent
    /// first.
    pub async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<ApiKeyRow>, AuthError> {
        self.keys
            .list_for_user(user_id)
            .await
            .map_err(AuthError::Storage)
    }

    /// Revoke a key by id. Returns `Ok(Some(row))` if the row
    /// was newly revoked, `Ok(None)` if it was already revoked
    /// OR doesn't belong to `user_id` (both flatten to the same
    /// response so an attacker probing for valid key ids can't
    /// tell "wrong user" from "wrong id").
    pub async fn revoke(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<ApiKeyRow>, AuthError> {
        self.keys
            .revoke(id, user_id, Utc::now())
            .await
            .map_err(AuthError::Storage)
    }

    /// Rotate a key: mint a new one with the same `name`,
    /// `scopes`, and `rate_limit_tier` as the source row, then
    /// revoke the source row. Returns the new [`IssuedApiKey`]
    /// on success. Returns `Ok(None)` if the source row doesn't
    /// exist OR is already revoked (idempotent — rotate-rotate
    /// is a no-op on the second call).
    ///
    /// Order is "issue new, then revoke old" specifically so a
    /// failure in `issue` (DB outage, retry exhaustion) leaves
    /// the caller with their original key intact. The previous
    /// "revoke first" order would have stranded the user with
    /// no valid key on `issue` failure. The trade-off is a
    /// brief overlap window where BOTH keys verify — that's
    /// acceptable here because (a) the user already trusts both
    /// (they chose to rotate), (b) the window is one DB round
    /// trip wide, and (c) if the final revoke itself fails the
    /// caller still gets `Ok(Some(new_key))` so they're not
    /// stuck without access, with a `warn!` logged for ops.
    pub async fn rotate(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<IssuedApiKey>, AuthError> {
        // Peek at the source row's metadata BEFORE issuing —
        // `list_for_user` returns every row owned by this user
        // (including revoked) so we filter on `revoked_at` to
        // reject the "already revoked" path with `Ok(None)`.
        // For realistic key counts (single digits to low tens
        // per user) the full-list scan is cheaper than adding a
        // dedicated `get_by_id` repo method that this is the
        // only caller of.
        let existing = self
            .keys
            .list_for_user(user_id)
            .await
            .map_err(AuthError::Storage)?
            .into_iter()
            .find(|r| r.id == id && r.revoked_at.is_none());
        let Some(existing) = existing else {
            return Ok(None);
        };
        // Issue first — if this fails the original key is still
        // valid because we haven't touched the source row yet.
        let issued = self
            .issue(user_id, existing.name, existing.scopes, existing.rate_limit_tier)
            .await?;
        // Best-effort revoke of the source row. We do NOT
        // propagate the error: the new key has been minted and
        // returning `Err` here would hide it from the caller
        // (gateway response → empty body → user has no idea a
        // new key exists). The brief overlap is the lesser
        // evil; the `warn!` flags the row for ops attention.
        if let Err(e) = self.keys.revoke(id, user_id, Utc::now()).await {
            tracing::warn!(
                error = %e,
                old_key_id = %id,
                new_key_id = %issued.id,
                "api key rotate: revoke of old key failed; new key minted successfully — old key remains valid until manually revoked"
            );
        }
        Ok(Some(issued))
    }
}

/// Trim each scope, drop empty entries, sort, and dedup. The
/// storage layer treats the scope set as a set (insertion
/// order isn't meaningful and downstream authorisation will
/// `contains`-check), so canonicalising the input here gives
/// every caller — web form, future MCP / CLI, batch importers —
/// the same row shape. Without this, `["read", "read", " ",
/// ""]` would persist verbatim and downstream "empty scopes
/// means no elevated capabilities" checks would silently
/// accept rows that don't actually express no capabilities.
fn normalise_scopes(scopes: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = scopes
        .into_iter()
        .filter_map(|s| {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_owned())
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Cheap shape pre-validation: literal `tdh_` prefix + exact
/// total length + every body char in the base64url alphabet.
/// Done in-Rust so a malformed key (random string, JWT, etc.)
/// can't trigger a wasted DB round trip OR an unintended
/// `Sha256::digest` of attacker-controlled bytes.
fn is_well_shaped(s: &str) -> bool {
    if s.len() != API_KEY_TOTAL_LEN {
        return false;
    }
    let Some(body) = s.strip_prefix(API_KEY_HUMAN_PREFIX) else {
        return false;
    };
    if body.len() != API_KEY_BODY_LEN {
        return false;
    }
    body.bytes()
        .all(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_shaped_accepts_minted_key() {
        // Construct a key with the exact shape the minter emits:
        // `tdh_` + 43 chars of base64url alphabet.
        let key = format!("{API_KEY_HUMAN_PREFIX}{}", "a".repeat(API_KEY_BODY_LEN));
        assert!(is_well_shaped(&key));
    }

    #[test]
    fn well_shaped_rejects_wrong_total_length() {
        assert!(!is_well_shaped("tdh_short"));
        assert!(!is_well_shaped(&format!(
            "{API_KEY_HUMAN_PREFIX}{}",
            "a".repeat(API_KEY_BODY_LEN + 5)
        )));
    }

    #[test]
    fn well_shaped_rejects_wrong_prefix() {
        let body = "a".repeat(API_KEY_BODY_LEN);
        // Same total length, different prefix.
        let bad = format!("tdz_{body}");
        assert!(!is_well_shaped(&bad));
    }

    #[test]
    fn well_shaped_rejects_non_base64url_body() {
        let mut body = "a".repeat(API_KEY_BODY_LEN);
        body.replace_range(5..6, "+"); // `+` is base64 but NOT base64url
        let bad = format!("{API_KEY_HUMAN_PREFIX}{body}");
        assert!(!is_well_shaped(&bad));
    }

    #[test]
    fn normalise_scopes_trims_drops_empties_dedups_sorts() {
        let input = vec![
            "  read  ".to_owned(),
            "write".to_owned(),
            String::new(),
            "   ".to_owned(),
            "read".to_owned(),
            " write ".to_owned(),
        ];
        let out = normalise_scopes(input);
        assert_eq!(out, vec!["read".to_owned(), "write".to_owned()]);
    }

    #[test]
    fn normalise_scopes_returns_empty_for_all_blank() {
        let input = vec![String::new(), "   ".to_owned(), "\t".to_owned()];
        assert!(normalise_scopes(input).is_empty());
    }

    #[test]
    fn normalise_scopes_preserves_distinct_entries() {
        let out = normalise_scopes(vec!["admin".into(), "read".into(), "write".into()]);
        assert_eq!(out, vec!["admin", "read", "write"]); // sorted
    }
}
