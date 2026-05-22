//! Email + password authentication (#4.2).
//!
//! Surface delivered in this milestone:
//!
//! - argon2id password hashing
//! - email verification + password reset via single-use magic
//!   links backed by `auth_tokens`
//! - provider-agnostic SMTP sender (works against Resend / Postmark
//!   / Mailgun / raw SMTP) plus a `LogMailer` fallback for
//!   personal-mode installs without SMTP credentials
//! - enumeration-safe response shape on every flow
//!
//! OAuth (#4.3 / #4.4) and session middleware (#4.5) compose on
//! top of [`AuthService`] without changing its surface — the
//! gateway's HTTP handlers in #4.5 take this service via `Arc<…>`
//! and translate its return values into cookies + JSON responses.

mod error;
mod mailer;
mod oauth;
mod password;
mod redact;
mod service;
mod token;

pub use error::AuthError;
pub use mailer::{LogMailer, MailFrom, MailKind, Mailer, MemoryMailer, SentMessage, SmtpMailer};
pub use oauth::{
    GitHubProvider, OAuthProvider, OAuthService, PkcePair, ProviderProfile, StartLogin, StateToken,
    TokenCipher, TokenCipherError, generate_pkce, generate_state, hash_state,
};
pub use password::{hash_password, verify_password};
pub use service::{
    AuthService, AuthenticatedUser, DEFAULT_MAX_INFLIGHT_SENDS, DEFAULT_RESET_TTL,
    DEFAULT_VERIFY_TTL, into_arc,
};
pub use token::{
    GeneratedToken, TOKEN_ENTROPY_BYTES, TOKEN_HASH_BYTES, digest_token, generate_token,
};
