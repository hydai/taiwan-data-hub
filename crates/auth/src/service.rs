//! Composition root for the v0.1 email + password flows.
//!
//! `AuthService` ties together a [`UserRepo`], an [`AuthTokenRepo`],
//! and a [`Mailer`]. Concrete types are generic so unit tests can
//! substitute in-memory fakes without taking a `dyn` dispatch hit.
//!
//! Six flows live here:
//!
//! 1. [`AuthService::register`] — create user + email a verify link.
//! 2. [`AuthService::resend_verification`] — mint + email a fresh
//!    verify link for an existing-but-unverified user (silent for
//!    unknown or already-verified addresses).
//! 3. [`AuthService::verify_email`] — redeem a verify token.
//! 4. [`AuthService::login`] — check password, return an
//!    [`AuthenticatedUser`] (the redacted DTO — no password hash).
//!    Session issuance lands in #4.5 — the gateway handler wraps
//!    this call and writes the cookie.
//! 5. [`AuthService::request_password_reset`] — email a reset link.
//! 6. [`AuthService::reset_password`] — redeem a reset token + set
//!    a new password.
//!
//! Enumeration-safety guarantees:
//!
//! - `register` returns the same shape whether or not the email was
//!   already taken (the call site converts both into the uniform
//!   "check your inbox" HTTP response).
//! - `login` runs an argon2 verify even when the email is unknown,
//!   so timing does not distinguish "user known" from "user
//!   unknown".
//! - `request_password_reset` looks up the user but ALWAYS returns
//!   `Ok(())`, so a probe can't tell which addresses are registered.
//! - SMTP send for verification + password-reset happens in a
//!   `tokio::spawn` background task, so the caller-visible response
//!   time does not depend on whether the recipient address exists
//!   (which would otherwise leak via SMTP latency variance). The
//!   tiny remaining timing edge is the `auth_tokens` insert that
//!   only happens for known users — a job queue in v0.2 will
//!   absorb that too.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use storage::{AuthTokenKind, AuthTokenRepo, User, UserRepo};
use url::Url;
use uuid::Uuid;

use crate::error::AuthError;
use crate::mailer::Mailer;
use crate::password::{hash_password, verify_dummy, verify_password};
use crate::token::{digest_token, generate_token};

/// Redacted user view returned to the auth-crate caller. Excludes
/// `password_hash` so a callback that hand-serialises the
/// authenticated user into a response (or a `tracing` event)
/// can't accidentally leak the credential material that
/// [`storage::User`] carries. Kept distinct from the DB-row type
/// so the redaction stays a compile-time invariant: future
/// fields on `storage::User` are opt-in here, not opt-out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedUser {
    pub id: Uuid,
    pub email: String,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl AuthenticatedUser {
    fn from_row(user: User) -> Self {
        Self {
            id: user.id,
            email: user.email,
            email_verified_at: user.email_verified_at,
            created_at: user.created_at,
        }
    }
}

/// Default lifetime for email-verification links (24 hours).
pub const DEFAULT_VERIFY_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Default lifetime for password-reset links (1 hour).
pub const DEFAULT_RESET_TTL: Duration = Duration::from_secs(60 * 60);

/// Composition root that performs the five v0.1 auth flows.
///
/// Generic over the repo + mailer traits so unit tests can swap
/// in `InMemoryUserRepo` etc. without a real Postgres. Wrapped in
/// `Arc` at call sites so the gateway can clone it cheaply for
/// each request.
///
/// The mailer is held in an `Arc<M>` internally so the SMTP send
/// can be `tokio::spawn`-ed without requiring `M: Clone`. Callers
/// pass a bare `M` to `new`; the wrap is invisible.
pub struct AuthService<R, T, M> {
    users: R,
    tokens: T,
    mailer: Arc<M>,
    public_base_url: Url,
    verify_ttl: Duration,
    reset_ttl: Duration,
}

impl<R, T, M> AuthService<R, T, M>
where
    R: UserRepo,
    T: AuthTokenRepo,
    M: Mailer + 'static,
{
    /// Build a service that mints magic links relative to
    /// `public_base_url` (e.g. `https://hub.example`). Verify
    /// links land at `<base>/auth/verify?token=…`; reset links at
    /// `<base>/auth/reset?token=…`.
    pub fn new(users: R, tokens: T, mailer: M, public_base_url: Url) -> Self {
        Self {
            users,
            tokens,
            mailer: Arc::new(mailer),
            public_base_url,
            verify_ttl: DEFAULT_VERIFY_TTL,
            reset_ttl: DEFAULT_RESET_TTL,
        }
    }

    /// Override the verification-link TTL (default 24h).
    #[must_use]
    pub fn with_verify_ttl(mut self, ttl: Duration) -> Self {
        self.verify_ttl = ttl;
        self
    }

    /// Override the password-reset-link TTL (default 1h).
    #[must_use]
    pub fn with_reset_ttl(mut self, ttl: Duration) -> Self {
        self.reset_ttl = ttl;
        self
    }

    /// Register a new account + email a verification link.
    ///
    /// Returns `AuthError::EmailTaken` only to internal callers;
    /// the HTTP boundary maps that to the same uniform response
    /// as success so address presence cannot be probed.
    pub async fn register(&self, email: &str, password: &str) -> Result<(), AuthError> {
        let hash = hash_password(password)?;
        let user = match self.users.insert_user(email, &hash).await {
            Ok(user) => user,
            Err(storage::StorageError::UniqueViolation(_)) => return Err(AuthError::EmailTaken),
            Err(e) => return Err(e.into()),
        };
        // Token insert / mail-send setup happens AFTER user insert.
        // If anything goes wrong, the row would otherwise sit in the
        // users table with no pending verification token — a retry
        // would hit "email taken" forever. Compensate by deleting
        // the row before returning the error.
        if let Err(err) = self.send_verification_link(&user).await {
            if let Err(cleanup_err) = self.users.delete_user(user.id).await {
                tracing::error!(
                    user_id = %user.id,
                    original_error = %err,
                    cleanup_error = %cleanup_err,
                    "register failed AND compensating delete failed; user row is orphaned",
                );
            } else {
                tracing::warn!(
                    user_id = %user.id,
                    error = %err,
                    "register failed after user insert; row deleted as compensation",
                );
            }
            return Err(err);
        }
        Ok(())
    }

    /// Resend the verification link for a user who already
    /// registered but didn't click in time. Returns `Ok(())` even
    /// when no such user exists, so probing remains uninformative.
    pub async fn resend_verification(&self, email: &str) -> Result<(), AuthError> {
        if let Some(user) = self.users.find_user_by_email(email).await?
            && user.email_verified_at.is_none()
        {
            self.send_verification_link(&user).await?;
        }
        Ok(())
    }

    async fn send_verification_link(&self, user: &User) -> Result<(), AuthError> {
        let token = generate_token();
        let expires = Utc::now()
            + chrono::Duration::from_std(self.verify_ttl).map_err(|e| {
                AuthError::InvalidConfig(format!("verify_ttl out of chrono range: {e}"))
            })?;
        self.tokens
            .insert_auth_token(user.id, AuthTokenKind::EmailVerify, &token.digest, expires)
            .await?;
        let link = magic_link(&self.public_base_url, "/auth/verify", &token.cleartext)?;
        self.spawn_send_verification(user.email.clone(), link);
        Ok(())
    }

    fn spawn_send_verification(&self, to: String, link: Url) {
        let mailer = Arc::clone(&self.mailer);
        let ttl = self.verify_ttl;
        tokio::spawn(async move {
            if let Err(err) = mailer.send_verification(&to, &link, ttl).await {
                tracing::error!(
                    error = %err,
                    "background verification mail send failed",
                );
            }
        });
    }

    fn spawn_send_password_reset(&self, to: String, link: Url) {
        let mailer = Arc::clone(&self.mailer);
        let ttl = self.reset_ttl;
        tokio::spawn(async move {
            if let Err(err) = mailer.send_password_reset(&to, &link, ttl).await {
                tracing::error!(
                    error = %err,
                    "background password-reset mail send failed",
                );
            }
        });
    }

    /// Redeem a verification token. Sets `email_verified_at = now()`
    /// on the matching user. The token is consumed atomically — a
    /// replay of the same link returns `AuthError::InvalidToken`.
    pub async fn verify_email(&self, cleartext_token: &str) -> Result<(), AuthError> {
        let digest = digest_token(cleartext_token);
        let user_id = self
            .tokens
            .consume_auth_token(AuthTokenKind::EmailVerify, &digest, Utc::now())
            .await?
            .ok_or(AuthError::InvalidToken)?;
        let _ = self.users.mark_email_verified(user_id).await?;
        Ok(())
    }

    /// Verify credentials. Returns an [`AuthenticatedUser`] on
    /// success (deliberately a redacted view, NOT [`storage::User`]
    /// — see the doc on [`AuthenticatedUser`] for why),
    /// `AuthError::InvalidCredentials` for either missing-user or
    /// wrong-password — both run an argon2 verify so the response
    /// timing is uniform.
    pub async fn login(&self, email: &str, password: &str) -> Result<AuthenticatedUser, AuthError> {
        if let Some(user) = self.users.find_user_by_email(email).await? {
            // A corrupt stored hash must NOT surface as a distinct
            // error to the caller — that would make "user exists
            // with malformed hash" distinguishable from "wrong
            // password" via HTTP status + timing, defeating the
            // enumeration-safety guarantee. Log loudly, return
            // InvalidCredentials.
            let matched = verify_password(password, &user.password_hash).unwrap_or_else(|err| {
                tracing::error!(
                    user_id = %user.id,
                    error = %err,
                    "stored password_hash is unparseable; treating login as a mismatch",
                );
                false
            });
            if matched {
                Ok(AuthenticatedUser::from_row(user))
            } else {
                Err(AuthError::InvalidCredentials)
            }
        } else {
            verify_dummy(password);
            Err(AuthError::InvalidCredentials)
        }
    }

    /// Email a password-reset magic link if the address is
    /// registered. Always returns `Ok(())` so probing the endpoint
    /// reveals nothing about which emails exist.
    pub async fn request_password_reset(&self, email: &str) -> Result<(), AuthError> {
        let Some(user) = self.users.find_user_by_email(email).await? else {
            return Ok(());
        };
        let token = generate_token();
        let expires = Utc::now()
            + chrono::Duration::from_std(self.reset_ttl).map_err(|e| {
                AuthError::InvalidConfig(format!("reset_ttl out of chrono range: {e}"))
            })?;
        self.tokens
            .insert_auth_token(
                user.id,
                AuthTokenKind::PasswordReset,
                &token.digest,
                expires,
            )
            .await?;
        let link = magic_link(&self.public_base_url, "/auth/reset", &token.cleartext)?;
        self.spawn_send_password_reset(user.email.clone(), link);
        Ok(())
    }

    /// Redeem a password-reset token + set a new password. The
    /// token is consumed atomically; an attacker who intercepts
    /// a single link cannot replay it.
    pub async fn reset_password(
        &self,
        cleartext_token: &str,
        new_password: &str,
    ) -> Result<(), AuthError> {
        let digest = digest_token(cleartext_token);
        let user_id = self
            .tokens
            .consume_auth_token(AuthTokenKind::PasswordReset, &digest, Utc::now())
            .await?
            .ok_or(AuthError::InvalidToken)?;
        let new_hash = hash_password(new_password)?;
        // A consumed token implies the owning user existed at
        // consume time. If the row is gone by now (admin delete
        // racing the reset, or a manual DB intervention), the
        // token has been wasted with nothing to update — bubble
        // that up so operators see the race instead of returning
        // a silent Ok.
        if !self.users.update_password_hash(user_id, &new_hash).await? {
            return Err(AuthError::Internal(format!(
                "reset_password consumed a token for user {user_id} but the user row is gone"
            )));
        }
        Ok(())
    }
}

/// Build a `<base>/<path>?token=<cleartext>` magic-link URL.
/// Extracted so verification + reset agree on the query-param
/// shape and any future change (e.g. anti-CSRF token) lands in
/// one place.
fn magic_link(base: &Url, path: &str, cleartext_token: &str) -> Result<Url, AuthError> {
    // A `base + path` failure is a service-configuration bug
    // (the operator passed a `public_base_url` that can't host
    // a sub-path), not a mail-delivery failure.
    let mut link = base
        .join(path)
        .map_err(|e| AuthError::InvalidConfig(format!("public_base_url + {path:?}: {e}")))?;
    link.query_pairs_mut().append_pair("token", cleartext_token);
    Ok(link)
}

/// Wrap an [`AuthService`] in `Arc` for sharing across async
/// handlers. Convenience for the gateway wiring in #4.5.
#[must_use]
pub fn into_arc<R, T, M>(svc: AuthService<R, T, M>) -> Arc<AuthService<R, T, M>>
where
    R: UserRepo,
    T: AuthTokenRepo,
    M: Mailer,
{
    Arc::new(svc)
}
