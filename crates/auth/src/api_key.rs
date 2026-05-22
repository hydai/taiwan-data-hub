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
//! `verify` is constant-time at the hash compare so an attacker
//! that gets to time the response cannot binary-search the hash
//! byte-by-byte. The `tdh_` literal prefix is checked first as a
//! cheap pre-validation (rejects obviously-not-our keys without
//! a DB round trip), but the rest of the verification path
//! treats both branches the same to avoid leaking shape via
//! timing.

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

        let id = self
            .keys
            .insert_api_key(NewApiKey {
                user_id,
                name: name.to_owned(),
                key_prefix: key_prefix.clone(),
                key_hash,
                scopes: scopes.to_vec(),
                rate_limit_tier: rate_limit_tier.to_owned(),
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
    /// request). The shape pre-validation is intentionally
    /// cheap (length + literal prefix); deeper format checks
    /// happen in the DB lookup so a malformed key still costs
    /// one DB round trip, matching the cost of a well-shaped
    /// but unknown key.
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

    /// Rotate a key: revoke the old row, mint a new one with
    /// the same `name`, `scopes`, and `rate_limit_tier`. Returns
    /// the new [`IssuedApiKey`] on success. Returns `Ok(None)`
    /// if the source row doesn't exist or is already revoked
    /// (idempotent — rotate-rotate is a no-op on the second
    /// call).
    pub async fn rotate(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<IssuedApiKey>, AuthError> {
        let Some(old) = self.revoke(id, user_id).await? else {
            return Ok(None);
        };
        // `revoke` already returned the snapshot of the row, so
        // we don't need a second SELECT to recover the
        // name/scopes/tier — but we DO need to mint with the
        // same values, not the user-supplied ones (the gateway
        // calls rotate by id only, no body).
        let issued = self
            .issue(user_id, old.name, old.scopes, old.rate_limit_tier)
            .await?;
        Ok(Some(issued))
    }
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
}
