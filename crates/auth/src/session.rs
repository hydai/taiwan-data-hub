//! Server-side session issuance + validation (#4.5).
//!
//! The cookie carries an OPAQUE 32-byte token (base64url-no-pad
//! encoded, 43 chars). The DB primary key is `sha256(token)`.
//! That gives us:
//!
//! - **No JWT trust boundary**: every request validates against
//!   the DB, so revocation is immediate. A stolen JWT-style
//!   token would stay valid until expiry; an opaque token gets
//!   killed at the next request after `revoke`.
//! - **DB leak ≠ token leak**: a dump yields only hashes, not
//!   working tokens.
//! - **Cookie-format stability**: the wire format is just
//!   `base64url(token)`; we don't carry an HMAC because the
//!   sha256-lookup IS the unforgeability boundary. An attacker
//!   would need to find a 32-byte preimage of a stored hash,
//!   which is computationally infeasible.
//!
//! The auth service produces [`IssuedSession`] (cookie value +
//! expiry) at login time and verifies inbound cookies via
//! [`SessionService::authenticate`].

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use rand::TryRngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use storage::{AuthenticatedSession, NewSession, SessionRepo};
use uuid::Uuid;

use crate::error::AuthError;

/// Default sliding-window session TTL. Issue #4.5 spec:
/// "Sliding window refresh on each request (max 14d total)" —
/// every authenticated request extends `expires_at` to
/// `now + DEFAULT_SESSION_TTL`. An active user effectively never
/// logs out; an idle user gets cleaned up after 14d.
pub const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// Cookie name the gateway middleware reads. Documented here so
/// the auth crate is the single source of truth even if the
/// gateway changes its routing.
pub const SESSION_COOKIE_NAME: &str = "tdh_session";

/// Number of random bytes in the opaque token. 32 bytes →
/// 256 bits of entropy → infeasible to brute-force the sha256
/// preimage.
const TOKEN_ENTROPY_BYTES: usize = 32;

/// Result of [`SessionService::issue`] — what the gateway puts
/// in the `Set-Cookie` header.
#[derive(Debug, Clone)]
pub struct IssuedSession {
    /// `base64url(token)`. The full cookie value, ready for
    /// `Set-Cookie`. The cleartext lives only here + in the
    /// client browser; the DB has only `sha256(token)`.
    pub cookie_value: String,
    /// Initial expiry. Anchors the cookie's `Max-Age` attribute
    /// at issue time; the sliding-window refresh in
    /// [`SessionService::authenticate`] advances this on each
    /// access (but the cookie itself isn't rewritten — the
    /// gateway re-issues `Set-Cookie` only on login / logout).
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

/// Composition root for the session lifecycle. Holds a session
/// repo (for the SQL surface) + the absolute TTL. Cheap to clone
/// (`Arc`-backed repo).
#[derive(Clone)]
pub struct SessionService {
    sessions: Arc<dyn SessionRepo>,
    ttl: Duration,
}

impl SessionService {
    pub fn new(sessions: Arc<dyn SessionRepo>) -> Self {
        Self {
            sessions,
            ttl: DEFAULT_SESSION_TTL,
        }
    }

    #[must_use]
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Mint a fresh session for `user_id`. Generates the opaque
    /// token, inserts the `sha256(token)` row, returns the
    /// cookie value + expiry for the gateway to set on the
    /// response.
    pub async fn issue(
        &self,
        user_id: Uuid,
        user_agent: Option<String>,
        ip_addr: Option<IpAddr>,
    ) -> Result<IssuedSession, AuthError> {
        let mut token_bytes = [0u8; TOKEN_ENTROPY_BYTES];
        OsRng
            .try_fill_bytes(&mut token_bytes)
            .expect("OsRng must provide entropy for session token");
        let cookie_value = URL_SAFE_NO_PAD.encode(token_bytes);
        let id_hash = Sha256::digest(token_bytes).to_vec();
        let now = Utc::now();
        let expires_at = now + self.chrono_ttl()?;
        self.sessions
            .insert_session(NewSession {
                id_hash,
                user_id,
                expires_at,
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
    ///   session. Side effects: `last_seen_at` is touched AND
    ///   `expires_at` is slid forward to `now + ttl` (the
    ///   sliding-window refresh per the #4.5 spec). The
    ///   returned `expires_at` reflects the post-slide value.
    /// - `Ok(None)` for missing / revoked / expired / malformed
    ///   token. The caller treats all of these as "anonymous"
    ///   and clears the cookie.
    ///
    /// A malformed cookie (bad base64, wrong length) does NOT
    /// surface as an error — the client may have stale data; we
    /// just want the request to land as anonymous.
    pub async fn authenticate(
        &self,
        cookie_value: &str,
    ) -> Result<Option<ValidatedSession>, AuthError> {
        let Some(id_hash) = hash_cookie(cookie_value) else {
            return Ok(None);
        };
        let now = Utc::now();
        let new_expires_at = now + self.chrono_ttl()?;
        Ok(self
            .sessions
            .touch_and_authenticate(&id_hash, now, new_expires_at)
            .await?
            .map(ValidatedSession::from))
    }

    /// `self.ttl` as a `chrono::Duration`. Shared by `issue` +
    /// `authenticate` so the conversion error path is identical
    /// at both call sites.
    fn chrono_ttl(&self) -> Result<chrono::Duration, AuthError> {
        chrono::Duration::from_std(self.ttl)
            .map_err(|e| AuthError::InvalidConfig(format!("session ttl out of chrono range: {e}")))
    }

    /// Revoke a specific session by cookie value. Returns `true`
    /// if the row was flipped (idempotent: already-revoked
    /// returns `false`). A malformed cookie returns `false`
    /// without touching the DB.
    pub async fn revoke(&self, cookie_value: &str) -> Result<bool, AuthError> {
        let Some(id_hash) = hash_cookie(cookie_value) else {
            return Ok(false);
        };
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

/// Decode + hash a cookie value into the DB lookup key. Returns
/// `None` for any malformed input so the caller treats it
/// uniformly as "no session" rather than a hard error.
fn hash_cookie(cookie_value: &str) -> Option<Vec<u8>> {
    let bytes = URL_SAFE_NO_PAD.decode(cookie_value).ok()?;
    if bytes.len() != TOKEN_ENTROPY_BYTES {
        return None;
    }
    Some(Sha256::digest(&bytes).to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_cookie_rejects_wrong_length() {
        // Empty, too short, too long all reject.
        assert!(hash_cookie("").is_none());
        // 16 bytes (`AAAA...` is base64 for zero bytes; need 22
        // chars to encode 16 bytes no-pad).
        assert!(hash_cookie(&"A".repeat(22)).is_none());
        // 64 bytes → 86 chars no-pad.
        assert!(hash_cookie(&"A".repeat(86)).is_none());
    }

    #[test]
    fn hash_cookie_rejects_non_base64() {
        // Padding char `=` is rejected by URL_SAFE_NO_PAD.
        assert!(hash_cookie(&format!("{}=", "A".repeat(42))).is_none());
        // Non-base64 char.
        assert!(hash_cookie(&"!".repeat(43)).is_none());
    }

    #[test]
    fn hash_cookie_round_trips_a_valid_token() {
        let token = URL_SAFE_NO_PAD.encode([7u8; 32]);
        let h = hash_cookie(&token).expect("valid token hashes");
        assert_eq!(h.len(), 32, "sha256 → 32 bytes");
        // Stable digest — same input → same hash.
        assert_eq!(h, hash_cookie(&token).unwrap());
    }
}
