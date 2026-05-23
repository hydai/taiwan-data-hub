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

mod api_key;
mod bookmarks;
mod comments;
mod error;
mod mailer;
mod moderation;
mod oauth;
mod password;
mod rate_limit;
mod ratings;
mod redact;
mod reports;
mod service;
mod session;
mod submission;
mod token;

pub use api_key::{
    ALLOWED_TIERS, API_KEY_HUMAN_PREFIX, ApiKeyService, DEFAULT_RATE_LIMIT_TIER, IssuedApiKey,
    VerifiedApiKey,
};
pub use bookmarks::{
    BookmarkService, COLLECTION_DESCRIPTION_MAX_LEN, COLLECTION_NAME_MAX_LEN,
    CollectionDenialReason, CollectionService, InputError as CollectionInputError,
};
pub use comments::{
    BodyError, CommentDenialReason, CommentService, DEFAULT_EDIT_WINDOW, MAX_COMMENT_BODY_LEN,
    RenderedComment,
};
pub use error::AuthError;
pub use mailer::{LogMailer, MailFrom, MailKind, Mailer, MemoryMailer, SentMessage, SmtpMailer};
pub use moderation::{Decision, ModerationDenialReason, ModerationService};
pub use oauth::{
    GitHubProvider, GoogleProvider, JwksCache, OAuthProvider, OAuthService, PkcePair,
    ProviderProfile, StartLogin, StateToken, TokenCipher, TokenCipherError, account_aad,
    generate_pkce, generate_state, hash_state,
};
pub use password::{hash_password, verify_password};
pub use rate_limit::{
    DEFAULT_IP_RPM, DEFAULT_QUERY_ROWS_RPM, InMemoryRateLimiter, PgRateLimiter, RateLimitOutcome,
    RateLimiter, WINDOW_SECONDS, tier_rpm,
};
pub use ratings::{
    MIN_ACCOUNT_AGE_FOR_RATING, RatingDenialReason, RatingService, SCORE_MAX, SCORE_MIN,
};
pub use reports::{
    REPORT_AUTO_HIDE_THRESHOLD, REPORT_BODY_MAX_LEN, ReportDenialReason, ReportService,
    ResolveDenialReason,
};
pub use service::{
    AuthService, AuthenticatedUser, DEFAULT_MAX_INFLIGHT_SENDS, DEFAULT_RESET_TTL,
    DEFAULT_VERIFY_TTL, into_arc,
};
pub use session::{
    DEFAULT_ABSOLUTE_MAX, DEFAULT_IDLE_TTL, IssuedSession, SESSION_COOKIE_NAME, SessionService,
    ValidatedSession,
};
pub use submission::{
    MAX_DESCRIPTION_LEN, MAX_NAME_LEN, MAX_URL_LEN, SubmissionPayload, SubmissionService,
    TITLE_MAX_LEN,
};
pub use token::{
    GeneratedToken, TOKEN_ENTROPY_BYTES, TOKEN_HASH_BYTES, digest_token, generate_token,
};
