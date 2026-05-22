//! Server-side session issuance + validation (#4.5).
//!
//! The cookie carries an OPAQUE 32-byte token + an HMAC tag:
//! `base64url(token).base64url(hmac_sha256(token, key))`. The DB
//! primary key is `sha256(token)`. That gives us:
//!
//! - **No JWT trust boundary**: every request validates against
//!   the DB, so revocation is immediate. A stolen JWT-style
//!   token would stay valid until expiry; an opaque token gets
//!   killed at the next request after `revoke`.
//! - **DB leak ≠ token leak**: a dump yields only hashes, not
//!   working tokens.
//! - **Signed cookie**: the HMAC tag lets the gateway reject
//!   malformed / tampered cookies cheaply (no DB roundtrip).
//!   Forging a valid pair without the HMAC key is
//!   computationally infeasible, and even with a forged pair an
//!   attacker would still need to find a token whose
//!   `sha256` matches a stored row.
//!
//! ## Sliding window + absolute cap
//!
//! Per the #4.5 spec ("Sliding window refresh on each request
//! (max 14d total)"), the service carries TWO durations:
//!
//! - `idle_ttl` — how far the idle window slides on each access.
//!   Default [`DEFAULT_IDLE_TTL`].
//! - `absolute_max` — hard cap on session lifetime from creation.
//!   Default [`DEFAULT_ABSOLUTE_MAX`].
//!
//! `expires_at` advances on each authenticated request to
//! `min(now + idle_ttl, absolute_expires_at)`. With the defaults
//! equal, that collapses to "fixed `absolute_max` from creation"
//! (matches the literal spec wording). Setting
//! `idle_ttl < absolute_max` gives the canonical Gmail-style
//! sliding-with-cap behavior — actively-used sessions live up to
//! `absolute_max`; idle sessions die after `idle_ttl`.

use std::net::IpAddr;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use rand::TryRngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use storage::{AuthenticatedSession, NewSession, SessionRepo, StorageError};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::error::AuthError;

/// Default idle-window TTL — how far `expires_at` slides on each
/// authenticated request.
pub const DEFAULT_IDLE_TTL: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// Default absolute lifetime cap — `absolute_expires_at` is set
/// to `now + DEFAULT_ABSOLUTE_MAX` at issue time and never
/// extended. Matches the spec's "max 14d total".
pub const DEFAULT_ABSOLUTE_MAX: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// Cookie name the gateway middleware reads. Documented here so
/// the auth crate is the single source of truth even if the
/// gateway changes its routing.
pub const SESSION_COOKIE_NAME: &str = "tdh_session";

/// Number of random bytes in the opaque token. 32 bytes →
/// 256 bits of entropy → infeasible to brute-force the sha256
/// preimage.
const TOKEN_ENTROPY_BYTES: usize = 32;

/// Minimum acceptable HMAC key length, in bytes. 32 bytes
/// matches the cookie token's entropy and the SHA-256 block
/// size, so the HMAC output isn't the bottleneck.
const MIN_HMAC_KEY_BYTES: usize = 32;

/// Separator between the token and the HMAC tag in the cookie
/// value. `.` is base64url-safe and not used by either part.
const COOKIE_TAG_SEPARATOR: char = '.';

/// Max retries on `StorageError::UniqueViolation` during
/// [`SessionService::issue`]. 32-byte entropy makes the
/// collision probability ~2^-256 per attempt, so any practical
/// collision implies a defective RNG; a small ceiling prevents
/// an infinite loop in that pathological case.
const ISSUE_MAX_ATTEMPTS: u32 = 3;

/// Expected length of the base64url-no-pad encoded token: 32
/// bytes → ceil(32 * 4 / 3) = 43 chars.
const TOKEN_BASE64_LEN: usize = 43;

/// Expected length of the base64url-no-pad encoded HMAC-SHA256
/// tag: 32 bytes → 43 chars (same as the token).
const TAG_BASE64_LEN: usize = 43;

/// SHA-256 / HMAC-SHA-256 output is always 32 bytes. Used to
/// reject a tag that decoded to the wrong length without
/// allocating the comparison buffer.
const TAG_DECODED_LEN: usize = 32;

/// Minimum acceptable session duration. `Max-Age=0` collides
/// with the cookie-clear semantics ([`build_clear_session_cookie`]
/// in the gateway crate), so we require both `idle_ttl` and
/// `absolute_max` to be at least one second.
const MIN_SESSION_DURATION: Duration = Duration::from_secs(1);

type HmacSha256 = Hmac<Sha256>;

/// Result of [`SessionService::issue`] — what the gateway puts
/// in the `Set-Cookie` header.
#[derive(Debug, Clone)]
pub struct IssuedSession {
    /// `<base64url(token)>.<base64url(hmac_sha256(token, key))>`.
    /// The full cookie value, ready for `Set-Cookie`. The
    /// cleartext token lives only here + in the client browser;
    /// the DB has only `sha256(token)`. The HMAC tag is what
    /// makes this a "signed cookie" per the #4.5 spec.
    pub cookie_value: String,
    /// Initial server-side idle expiry: `min(now + idle_ttl,
    /// absolute_expires_at)`. Stored on the session row and
    /// advanced by the sliding-window refresh in
    /// [`SessionService::authenticate`] (the cookie itself
    /// isn't rewritten — the gateway re-issues `Set-Cookie`
    /// only on login / logout).
    ///
    /// This is NOT the cookie's `Max-Age`. The browser-side
    /// lifetime comes from
    /// [`SessionService::cookie_max_age_seconds`] which uses
    /// `absolute_max`, so the cookie evicts at the hard cap
    /// regardless of how short or long the idle window is. The
    /// two values are intentionally decoupled: the idle expiry
    /// gates server-side authentication, the Max-Age gates
    /// browser-side eviction, and the absolute cap is the
    /// shared ceiling that keeps them consistent.
    pub expires_at: DateTime<Utc>,
}

/// What [`SessionService::authenticate`] returns for a valid
/// cookie. The gateway middleware inserts this into the request
/// extensions; downstream handlers extract it via the axum
/// `Extension<ValidatedSession>` extractor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedSession {
    pub user_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl From<AuthenticatedSession> for ValidatedSession {
    fn from(row: AuthenticatedSession) -> Self {
        Self {
            user_id: row.user_id,
            created_at: row.created_at,
            expires_at: row.expires_at,
        }
    }
}

/// Composition root for the session lifecycle.
///
/// Carries the session repo (for the SQL surface), the
/// sliding/absolute durations, and the HMAC signing key. Cheap
/// to clone:
///
/// - `sessions` is `Arc<dyn …>` (refcount bump).
/// - `idle_ttl` + `absolute_max` are `Duration` (`Copy`).
/// - `hmac_key` is `Arc<[u8]>` so the key bytes are NOT
///   re-allocated on clone (refcount bump). Important because
///   axum middleware shares the service via `State<Arc<…>>` +
///   `Clone`-per-request.
///
/// `Debug` is custom: the HMAC key is NEVER printed.
#[derive(Clone)]
pub struct SessionService {
    sessions: Arc<dyn SessionRepo>,
    idle_ttl: Duration,
    absolute_max: Duration,
    /// Symmetric HMAC key for cookie signing. Loaded from env at
    /// startup; min length [`MIN_HMAC_KEY_BYTES`]. Stored as
    /// `Arc<[u8]>` so `SessionService::clone` is O(1).
    hmac_key: Arc<[u8]>,
}

impl std::fmt::Debug for SessionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionService")
            .field("idle_ttl", &self.idle_ttl)
            .field("absolute_max", &self.absolute_max)
            // HMAC key length only — never the bytes themselves.
            .field("hmac_key_len", &self.hmac_key.len())
            .finish_non_exhaustive()
    }
}

impl SessionService {
    /// Build a service with default `idle_ttl` + `absolute_max`
    /// ([`DEFAULT_IDLE_TTL`] / [`DEFAULT_ABSOLUTE_MAX`]). Errors
    /// if `hmac_key` is shorter than [`MIN_HMAC_KEY_BYTES`] —
    /// configuration error, not a runtime case.
    ///
    /// The key is accepted as `impl Into<Arc<[u8]>>` so callers
    /// can hand in either a `Vec<u8>` (env-loaded) or a literal
    /// fixed-size byte slice (tests).
    pub fn new(
        sessions: Arc<dyn SessionRepo>,
        hmac_key: impl Into<Arc<[u8]>>,
    ) -> Result<Self, AuthError> {
        let hmac_key = hmac_key.into();
        if hmac_key.len() < MIN_HMAC_KEY_BYTES {
            return Err(AuthError::InvalidConfig(format!(
                "SESSION_HMAC_KEY must be >= {MIN_HMAC_KEY_BYTES} bytes, got {}",
                hmac_key.len()
            )));
        }
        Ok(Self {
            sessions,
            idle_ttl: DEFAULT_IDLE_TTL,
            absolute_max: DEFAULT_ABSOLUTE_MAX,
            hmac_key,
        })
    }

    /// Override `idle_ttl`. Panics if `ttl < 1s` — sub-second
    /// idle windows are nonsensical and would round to
    /// `Max-Age=0` (immediate cookie deletion). Builder method,
    /// so panic vs Result is a wash; we pick panic for chain
    /// ergonomics and because a misconfigured TTL is a startup
    /// programming error.
    #[must_use]
    pub fn with_idle_ttl(mut self, ttl: Duration) -> Self {
        assert!(
            ttl >= MIN_SESSION_DURATION,
            "idle_ttl must be >= 1s, got {ttl:?}"
        );
        self.idle_ttl = ttl;
        self
    }

    /// Override `absolute_max`. Same `>= 1s` requirement as
    /// [`Self::with_idle_ttl`].
    #[must_use]
    pub fn with_absolute_max(mut self, max: Duration) -> Self {
        assert!(
            max >= MIN_SESSION_DURATION,
            "absolute_max must be >= 1s, got {max:?}"
        );
        self.absolute_max = max;
        self
    }

    /// Cookie `Max-Age` value the gateway should emit. The
    /// browser-side cookie lifetime tracks the hard cap so
    /// eviction happens at the same time the server stops
    /// accepting the session — never before.
    ///
    /// Returns [`NonZeroU64`] so a future caller cannot
    /// accidentally pass `0` into [`build_session_cookie`] (which
    /// browsers interpret as "delete this cookie immediately",
    /// i.e. an instant silent logout). The `>= 1s` invariant is
    /// already enforced by [`Self::with_absolute_max`] and by
    /// [`DEFAULT_ABSOLUTE_MAX`]; the `expect` below would only
    /// fire if a constructor regressed past those guards, in
    /// which case panicking at startup beats serving a broken
    /// Set-Cookie header.
    #[must_use]
    pub fn cookie_max_age_seconds(&self) -> NonZeroU64 {
        NonZeroU64::new(self.absolute_max.as_secs())
            .expect("absolute_max >= 1s enforced by with_absolute_max / DEFAULT_ABSOLUTE_MAX")
    }

    /// Mint a fresh session for `user_id`. Generates the opaque
    /// token, HMACs it under [`Self::hmac_key`], inserts the
    /// `sha256(token)` row, returns `<token>.<tag>` + the
    /// initial idle expiry.
    ///
    /// Retries up to [`ISSUE_MAX_ATTEMPTS`] times on
    /// [`StorageError::UniqueViolation`] (PK collision on
    /// `sha256(token)`). 32 bytes of `OsRng` entropy makes
    /// a real collision astronomical (~2^-256 per try), so
    /// hitting the retry path twice in a row implies a defective
    /// RNG — surface that as an error rather than spinning.
    pub async fn issue(
        &self,
        user_id: Uuid,
        user_agent: Option<String>,
        ip_addr: Option<IpAddr>,
    ) -> Result<IssuedSession, AuthError> {
        for attempt in 1..=ISSUE_MAX_ATTEMPTS {
            match self
                .try_issue_once(user_id, user_agent.clone(), ip_addr)
                .await
            {
                Ok(issued) => return Ok(issued),
                Err(AuthError::Storage(StorageError::UniqueViolation(constraint))) => {
                    // Final attempt failing on collision means
                    // the RNG is busted (or the table is in a
                    // genuinely impossible state). Either way,
                    // surface as `Internal` rather than passing
                    // the raw `UniqueViolation` up — the caller
                    // can't do anything useful with the latter
                    // and "tried N times" is the actionable bit.
                    if attempt == ISSUE_MAX_ATTEMPTS {
                        return Err(AuthError::Internal(format!(
                            "session insert hit {ISSUE_MAX_ATTEMPTS} consecutive PK collisions \
                            on {constraint} — likely a defective RNG"
                        )));
                    }
                    tracing::warn!(
                        attempt,
                        constraint,
                        "session insert hit PK collision; retrying with fresh token"
                    );
                }
                Err(e) => return Err(e),
            }
        }
        unreachable!("loop bounds are 1..=ISSUE_MAX_ATTEMPTS and every iteration returns");
    }

    /// One attempt at minting + inserting a session. Returns the
    /// `UniqueViolation` to the caller so [`Self::issue`] can
    /// decide whether to retry.
    async fn try_issue_once(
        &self,
        user_id: Uuid,
        user_agent: Option<String>,
        ip_addr: Option<IpAddr>,
    ) -> Result<IssuedSession, AuthError> {
        let mut token_bytes = [0u8; TOKEN_ENTROPY_BYTES];
        OsRng
            .try_fill_bytes(&mut token_bytes)
            .expect("OsRng must provide entropy for session token");
        let id_hash = Sha256::digest(token_bytes).to_vec();
        let cookie_value = self.sign_token_bytes(&token_bytes);
        let now = Utc::now();
        let absolute_expires_at = now + Self::chrono_duration(self.absolute_max)?;
        let idle_expiry = now + Self::chrono_duration(self.idle_ttl)?;
        let expires_at = idle_expiry.min(absolute_expires_at);
        self.sessions
            .insert_session(NewSession {
                id_hash,
                user_id,
                // Single wall-clock source for the whole row.
                // `created_at` (and `last_seen_at`, which the
                // repo binds from the same value) shares the
                // clock used for the expiries below — keeps
                // `created_at <= last_seen_at < expires_at <
                // absolute_expires_at` true even under app/DB
                // skew.
                created_at: now,
                expires_at,
                absolute_expires_at,
                user_agent,
                ip_addr,
            })
            .await?;
        Ok(IssuedSession {
            cookie_value,
            expires_at,
        })
    }

    /// Validate an inbound cookie value. Returns:
    ///
    /// - `Ok(Some(session))` for a live, unrevoked, unexpired
    ///   session that ALSO passes the HMAC tag check. Side
    ///   effects: `last_seen_at` is touched AND `expires_at` is
    ///   slid forward to `min(now + idle_ttl,
    ///   absolute_expires_at)` (the sliding-window refresh with
    ///   absolute cap per the #4.5 spec).
    /// - `Ok(None)` for missing / revoked / expired / malformed /
    ///   tampered-tag token. The caller treats all of these as
    ///   "anonymous" and clears the cookie.
    ///
    /// A malformed cookie (bad base64, wrong length, bad HMAC)
    /// does NOT surface as an error — the client may have stale
    /// data; we just want the request to land as anonymous.
    pub async fn authenticate(
        &self,
        cookie_value: &str,
    ) -> Result<Option<ValidatedSession>, AuthError> {
        let Some(token_bytes) = self.verify_cookie(cookie_value) else {
            return Ok(None);
        };
        let id_hash = Sha256::digest(token_bytes).to_vec();
        let now = Utc::now();
        let new_expires_at = now + Self::chrono_duration(self.idle_ttl)?;
        Ok(self
            .sessions
            .touch_and_authenticate(&id_hash, now, new_expires_at)
            .await?
            .map(ValidatedSession::from))
    }

    /// A `Duration` as `chrono::Duration`. Folds the conversion
    /// error to `InvalidConfig` so callers don't need to know
    /// about the `OutOfRange` shape.
    fn chrono_duration(d: Duration) -> Result<chrono::Duration, AuthError> {
        chrono::Duration::from_std(d)
            .map_err(|e| AuthError::InvalidConfig(format!("session duration out of range: {e}")))
    }

    /// HMAC-SHA-256 of `token_bytes` under [`Self::hmac_key`],
    /// returned as a base64url-no-pad string.
    fn hmac_tag(&self, token_bytes: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.hmac_key)
            .expect("HmacSha256 accepts any key length we accept at construction");
        mac.update(token_bytes);
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }

    /// `<base64url(token)>.<base64url(hmac)>` — the wire format.
    fn sign_token_bytes(&self, token_bytes: &[u8]) -> String {
        let token = URL_SAFE_NO_PAD.encode(token_bytes);
        let tag = self.hmac_tag(token_bytes);
        format!("{token}{COOKIE_TAG_SEPARATOR}{tag}")
    }

    /// Parse, decode, and HMAC-verify a cookie value. Returns
    /// the raw 32-byte token on success, `None` on any
    /// malformation (wrong shape, bad base64, wrong length,
    /// invalid tag).
    ///
    /// Cheap pre-validation rejects oversized / malformed
    /// cookies BEFORE base64-decoding so an attacker can't
    /// burn server CPU by sending a megabyte of "cookie": the
    /// token part is exactly 43 chars, the tag part is exactly
    /// 43 chars, both checks happen on the byte counts before
    /// any allocation.
    fn verify_cookie(&self, cookie_value: &str) -> Option<[u8; TOKEN_ENTROPY_BYTES]> {
        let (token_b64, tag_b64) = cookie_value.split_once(COOKIE_TAG_SEPARATOR)?;
        if token_b64.len() != TOKEN_BASE64_LEN || tag_b64.len() != TAG_BASE64_LEN {
            return None;
        }
        let token_bytes = URL_SAFE_NO_PAD.decode(token_b64).ok()?;
        if token_bytes.len() != TOKEN_ENTROPY_BYTES {
            return None;
        }
        let supplied_tag = URL_SAFE_NO_PAD.decode(tag_b64).ok()?;
        if supplied_tag.len() != TAG_DECODED_LEN {
            return None;
        }
        let expected_tag = {
            let mut mac = HmacSha256::new_from_slice(&self.hmac_key).expect("hmac key valid");
            mac.update(&token_bytes);
            mac.finalize().into_bytes()
        };
        // Constant-time compare via `subtle` — defends against
        // timing attacks that walk the tag byte-by-byte.
        if supplied_tag.ct_eq(expected_tag.as_slice()).into() {
            let mut out = [0u8; TOKEN_ENTROPY_BYTES];
            out.copy_from_slice(&token_bytes);
            Some(out)
        } else {
            None
        }
    }

    /// Revoke a specific session by cookie value. Returns `true`
    /// if the row was flipped (idempotent: already-revoked
    /// returns `false`). A malformed cookie returns `false`
    /// without touching the DB.
    pub async fn revoke(&self, cookie_value: &str) -> Result<bool, AuthError> {
        let Some(token_bytes) = self.verify_cookie(cookie_value) else {
            return Ok(false);
        };
        let id_hash = Sha256::digest(token_bytes).to_vec();
        let now = Utc::now();
        Ok(self.sessions.revoke_session(&id_hash, now).await?)
    }

    /// Revoke every active session belonging to `user_id`. Used
    /// by "log out everywhere" and by the password-change flow.
    /// Returns the count of rows newly revoked.
    pub async fn revoke_all_for_user(&self, user_id: Uuid) -> Result<u64, AuthError> {
        let now = Utc::now();
        Ok(self
            .sessions
            .revoke_all_sessions_for_user(user_id, now)
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use storage::{AuthenticatedSession, StorageError};

    /// Bare-bones session repo for the cookie-format unit tests.
    /// The full integration tests in `tests/session.rs` use a
    /// richer fake; here we just need something that satisfies
    /// the trait bound so we can construct a `SessionService`.
    #[derive(Default)]
    struct NullRepo;
    #[async_trait::async_trait]
    impl SessionRepo for NullRepo {
        async fn insert_session(&self, _: NewSession) -> Result<(), StorageError> {
            Ok(())
        }
        async fn touch_and_authenticate(
            &self,
            _: &[u8],
            _: DateTime<Utc>,
            _: DateTime<Utc>,
        ) -> Result<Option<AuthenticatedSession>, StorageError> {
            Ok(None)
        }
        async fn revoke_session(&self, _: &[u8], _: DateTime<Utc>) -> Result<bool, StorageError> {
            Ok(false)
        }
        async fn revoke_all_sessions_for_user(
            &self,
            _: Uuid,
            _: DateTime<Utc>,
        ) -> Result<u64, StorageError> {
            Ok(0)
        }
    }

    fn svc() -> SessionService {
        SessionService::new(Arc::new(NullRepo), vec![7u8; MIN_HMAC_KEY_BYTES])
            .expect("hmac key valid")
    }

    #[test]
    fn rejects_hmac_key_shorter_than_minimum() {
        let err =
            SessionService::new(Arc::new(NullRepo), vec![7u8; MIN_HMAC_KEY_BYTES - 1]).unwrap_err();
        assert!(matches!(err, AuthError::InvalidConfig(_)));
    }

    #[test]
    fn verify_cookie_round_trips_signed_token() {
        let s = svc();
        let token_bytes = [42u8; TOKEN_ENTROPY_BYTES];
        let cookie = s.sign_token_bytes(&token_bytes);
        let recovered = s.verify_cookie(&cookie).expect("valid cookie verifies");
        assert_eq!(recovered, token_bytes);
    }

    #[test]
    fn verify_cookie_rejects_tampered_tag() {
        let s = svc();
        let cookie = s.sign_token_bytes(&[42u8; TOKEN_ENTROPY_BYTES]);
        // Flip one char in the tag; HMAC compare must fail.
        let mut bad = cookie.clone();
        let last = bad.pop().unwrap();
        bad.push(if last == 'A' { 'B' } else { 'A' });
        assert!(s.verify_cookie(&bad).is_none());
    }

    #[test]
    fn verify_cookie_rejects_tampered_token_with_old_tag() {
        let s = svc();
        let cookie = s.sign_token_bytes(&[42u8; TOKEN_ENTROPY_BYTES]);
        let (token_b64, tag_b64) = cookie.split_once('.').unwrap();
        // Different token, same tag — HMAC mismatch.
        let other_token = URL_SAFE_NO_PAD.encode([0u8; TOKEN_ENTROPY_BYTES]);
        let bad = format!("{other_token}.{tag_b64}");
        let _ = token_b64;
        assert!(s.verify_cookie(&bad).is_none());
    }

    #[test]
    fn verify_cookie_rejects_missing_tag_separator() {
        let s = svc();
        assert!(s.verify_cookie("nosepheretokencheck").is_none());
        // Empty.
        assert!(s.verify_cookie("").is_none());
    }

    #[test]
    fn verify_cookie_rejects_wrong_token_length() {
        let s = svc();
        // 16 bytes of token (22 b64 chars) + a (now-mismatched)
        // tag fails the length check before the HMAC compare.
        let short_token = URL_SAFE_NO_PAD.encode([0u8; 16]);
        let tag = "AAAA";
        let bad = format!("{short_token}.{tag}");
        assert!(s.verify_cookie(&bad).is_none());
    }

    #[test]
    fn verify_cookie_rejects_non_base64() {
        let s = svc();
        // `!` is not a base64url char.
        assert!(
            s.verify_cookie(&format!("!!!.{}", "A".repeat(43)))
                .is_none()
        );
        assert!(
            s.verify_cookie(&format!("{}.!!!", "A".repeat(43)))
                .is_none()
        );
    }
}
