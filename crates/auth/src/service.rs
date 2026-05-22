//! Composition root for the v0.1 email + password flows.
//!
//! `AuthService` ties together a [`UserRepo`], an [`AuthTokenRepo`],
//! and a [`Mailer`]. Concrete types are generic so unit tests can
//! substitute in-memory fakes without taking a `dyn` dispatch hit.
//!
//! Five flows live here:
//!
//! 1. [`AuthService::register`] — create user + email a verify link.
//! 2. [`AuthService::verify_email`] — redeem a verify token.
//! 3. [`AuthService::login`] — check password, return the `User`.
//!    (Session issuance lands in #4.5 — the gateway handler wraps
//!    this call and writes the cookie.)
//! 4. [`AuthService::request_password_reset`] — email a reset link.
//! 5. [`AuthService::reset_password`] — redeem a reset token + set
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

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use storage::{AuthTokenKind, AuthTokenRepo, User, UserRepo};
use url::Url;

use crate::error::AuthError;
use crate::mailer::Mailer;
use crate::password::{hash_password, verify_dummy, verify_password};
use crate::token::{digest_token, generate_token};

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
pub struct AuthService<R, T, M> {
    users: R,
    tokens: T,
    mailer: M,
    public_base_url: Url,
    verify_ttl: Duration,
    reset_ttl: Duration,
}

impl<R, T, M> AuthService<R, T, M>
where
    R: UserRepo,
    T: AuthTokenRepo,
    M: Mailer,
{
    /// Build a service that mints magic links relative to
    /// `public_base_url` (e.g. `https://hub.example`). Verify
    /// links land at `<base>/auth/verify?token=…`; reset links at
    /// `<base>/auth/reset?token=…`.
    pub fn new(users: R, tokens: T, mailer: M, public_base_url: Url) -> Self {
        Self {
            users,
            tokens,
            mailer,
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
        self.send_verification_link(&user).await
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
                AuthError::PasswordHash(format!("verify_ttl out of chrono range: {e}"))
            })?;
        self.tokens
            .insert_auth_token(user.id, AuthTokenKind::EmailVerify, &token.digest, expires)
            .await?;
        let link = magic_link(&self.public_base_url, "/auth/verify", &token.cleartext)?;
        self.mailer.send_verification(&user.email, &link).await
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

    /// Verify credentials. Returns the [`User`] row on success,
    /// `AuthError::InvalidCredentials` for either missing-user or
    /// wrong-password — both run an argon2 verify so the response
    /// timing is uniform.
    pub async fn login(&self, email: &str, password: &str) -> Result<User, AuthError> {
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
                Ok(user)
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
                AuthError::PasswordHash(format!("reset_ttl out of chrono range: {e}"))
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
        self.mailer.send_password_reset(&user.email, &link).await
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
        let _ = self.users.update_password_hash(user_id, &new_hash).await?;
        Ok(())
    }
}

/// Build a `<base>/<path>?token=<cleartext>` magic-link URL.
/// Extracted so verification + reset agree on the query-param
/// shape and any future change (e.g. anti-CSRF token) lands in
/// one place.
fn magic_link(base: &Url, path: &str, cleartext_token: &str) -> Result<Url, AuthError> {
    let mut link = base
        .join(path)
        .map_err(|e| AuthError::Mailer(format!("public_base_url + {path:?}: {e}")))?;
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
