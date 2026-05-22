//! Public error type for the auth crate.
//!
//! Variants are deliberately coarse-grained at the callee boundary
//! so HTTP handlers and tests can pattern-match without knowing the
//! internal SQL / Argon2 / SMTP / token shapes. Anything carrying
//! sensitive bytes (passwords, raw tokens) MUST go through these
//! variants — leaking via `Debug` from a lower-level error would
//! land in a `tracing` event.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    /// A new registration tried to use an already-taken email.
    /// Surfaced via a uniform "if such email exists, you'll get a
    /// verification mail" response at the HTTP boundary so the
    /// presence of the address cannot be probed.
    #[error("email already in use")]
    EmailTaken,
    /// Login failed — email unknown or password mismatched. One
    /// variant for both cases so timing + body do not distinguish
    /// "user does not exist" from "wrong password".
    #[error("invalid credentials")]
    InvalidCredentials,
    /// Email verification or password-reset token was missing,
    /// already consumed, or past `expires_at`.
    #[error("token is invalid or expired")]
    InvalidToken,
    /// Argon2id rejected the password format (e.g. UTF-8 too long
    /// for its memory parameters) or the stored hash. Should be
    /// impossible for fresh registrations; surfaced for clarity if
    /// the column is hand-edited or a future migration corrupts it.
    #[error("password hashing failed: {0}")]
    PasswordHash(String),
    /// Mailer-side failure surfaced to the caller. Covers two
    /// distinct shapes:
    ///
    /// 1. **Mailer construction / parse errors** — invalid SMTP
    ///    URL, malformed `From:` address, or a body the builder
    ///    rejects. These reach the caller because they happen
    ///    before the background send is spawned.
    /// 2. **Background SMTP delivery failures** — typically
    ///    logged via `tracing::error!` inside the spawned task
    ///    and NOT returned to the caller (the response shape
    ///    needs to stay uniform regardless of whether the
    ///    recipient exists). This variant is the type the
    ///    spawned task constructs before deciding to log+swallow.
    #[error("mailer error: {0}")]
    Mailer(String),
    /// A database call failed. Wrapped so callers don't depend on
    /// sqlx directly.
    #[error(transparent)]
    Storage(#[from] storage::StorageError),
    /// Service configuration is impossible: e.g. a TTL outside
    /// `chrono::Duration`'s representable range. Distinct from
    /// `PasswordHash` (cryptographic) and `Storage` (backend
    /// reachability) so operator-facing alerts can route the
    /// "this binary was misconfigured" case separately.
    #[error("auth service is misconfigured: {0}")]
    InvalidConfig(String),
    /// An invariant the auth crate relies on was violated after
    /// the DB call succeeded — e.g. a verified reset token whose
    /// owning user row has since been deleted. Always indicates
    /// a data-integrity bug or an admin-side race; HTTP boundary
    /// maps to 500.
    #[error("auth invariant violated: {0}")]
    Internal(String),
    /// OAuth callback (#4.3, #4.4) received a `state` that didn't
    /// match a known pending row, came back for a different
    /// provider than the one that issued it, or carried a
    /// mismatched `redirect_uri`. Almost always the user
    /// abandoned the redirect or an attacker swapped the URL.
    #[error("oauth state is invalid or expired")]
    InvalidState,
    /// OAuth token-exchange or profile-fetch HTTP round-trip
    /// failed. Wraps the upstream error text without leaking the
    /// access token (the token-exchange path never has the
    /// access token cleartext yet on the failure branch).
    #[error("oauth exchange failed: {0}")]
    OAuthExchange(String),
}
