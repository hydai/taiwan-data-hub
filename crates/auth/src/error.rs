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
    /// SMTP delivery failed. The HTTP boundary still returns a
    /// uniform 202 so the response shape does not reveal whether
    /// the recipient address exists.
    #[error("could not send email: {0}")]
    Mailer(String),
    /// A database call failed. Wrapped so callers don't depend on
    /// sqlx directly.
    #[error(transparent)]
    Storage(#[from] storage::StorageError),
}
