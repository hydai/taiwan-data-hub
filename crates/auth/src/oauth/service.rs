//! Composition root for the OAuth 2.1 flow.
//!
//! `OAuthService` ties together a provider impl, the
//! `oauth_states` + `oauth_accounts` repos, the user repo, and
//! the AES-GCM cipher. Two flows:
//!
//! 1. [`OAuthService::start_login`] mints a PKCE pair + CSRF
//!    state, persists them in `oauth_states`, and returns the
//!    redirect-to URL.
//! 2. [`OAuthService::finish_login`] consumes the matching
//!    `oauth_states` row, exchanges the code, fetches the
//!    provider profile, and either creates a new user or links
//!    the OAuth account to an existing one by verified email.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use storage::{
    NewOAuthAccount, OAuthAccountRepo, OAuthPendingState, OAuthStateRepo, StorageError, UserRepo,
};
use url::Url;
use uuid::Uuid;

use crate::error::AuthError;
use crate::oauth::crypto::TokenCipher;
use crate::oauth::pkce::generate_pkce;
use crate::oauth::provider::OAuthProvider;
use crate::oauth::state::{generate_state, hash_state};
use crate::service::AuthenticatedUser;

/// Default lifetime for a pending OAuth state. The user has this
/// long to click "authorize" on the provider's consent screen
/// before the state expires.
pub const DEFAULT_STATE_TTL: Duration = Duration::from_secs(10 * 60);

/// Result of [`OAuthService::start_login`] — the URL to redirect
/// the user to.
#[derive(Debug, Clone)]
pub struct StartLogin {
    pub redirect_to: Url,
}

/// Composition root for #4.3 (and #4.4 once Google lands).
///
/// Generic over the provider so tests can swap a fake without
/// hitting GitHub. The state / account / user repos are taken as
/// `Arc<dyn …>` so callers can share a single `Storage` across
/// multiple services.
pub struct OAuthService<P> {
    provider: P,
    states: Arc<dyn OAuthStateRepo>,
    accounts: Arc<dyn OAuthAccountRepo>,
    users: Arc<dyn UserRepo>,
    cipher: TokenCipher,
    state_ttl: Duration,
}

impl<P> OAuthService<P>
where
    P: OAuthProvider,
{
    pub fn new(
        provider: P,
        states: Arc<dyn OAuthStateRepo>,
        accounts: Arc<dyn OAuthAccountRepo>,
        users: Arc<dyn UserRepo>,
        cipher: TokenCipher,
    ) -> Self {
        Self {
            provider,
            states,
            accounts,
            users,
            cipher,
            state_ttl: DEFAULT_STATE_TTL,
        }
    }

    #[must_use]
    pub fn with_state_ttl(mut self, ttl: Duration) -> Self {
        self.state_ttl = ttl;
        self
    }

    /// Begin a login. Mints PKCE + CSRF state, persists, returns
    /// the redirect URL.
    pub async fn start_login(&self, redirect_uri: &str) -> Result<StartLogin, AuthError> {
        let pkce = generate_pkce();
        let state = generate_state();
        let expires_at = Utc::now()
            + chrono::Duration::from_std(self.state_ttl).map_err(|e| {
                AuthError::InvalidConfig(format!("oauth state_ttl out of chrono range: {e}"))
            })?;
        self.states
            .insert_oauth_state(OAuthPendingState {
                state_hash: state.digest,
                code_verifier: pkce.code_verifier,
                provider: self.provider.name().to_owned(),
                redirect_uri: redirect_uri.to_owned(),
                expires_at,
            })
            .await?;
        let redirect_to =
            self.provider
                .authorize_url(&state.cleartext, &pkce.code_challenge, redirect_uri)?;
        Ok(StartLogin { redirect_to })
    }

    /// Complete a login. Consumes the matching pending state,
    /// exchanges the code, links the account to a user (creating
    /// one by email if no match), and returns an
    /// `AuthenticatedUser`.
    ///
    /// On any error after the state is consumed, the state is
    /// already gone — the user has to start a fresh authorize
    /// round trip. That's the v0.1 trade-off: simpler than
    /// keeping a transaction across the network round trip to
    /// the provider.
    pub async fn finish_login(
        &self,
        code: &str,
        state_cleartext: &str,
        redirect_uri: &str,
    ) -> Result<AuthenticatedUser, AuthError> {
        let now = Utc::now();
        let state_hash = hash_state(state_cleartext);
        let pending = self
            .states
            .consume_oauth_state(&state_hash, now)
            .await?
            .ok_or(AuthError::InvalidState)?;
        if pending.provider != self.provider.name() {
            return Err(AuthError::InvalidState);
        }
        if pending.redirect_uri != redirect_uri {
            // The redirect_uri the callback was invoked with must
            // match the one we asked the provider to call back —
            // otherwise an attacker who can pin a victim to a
            // hostile callback URL could replay the code.
            return Err(AuthError::InvalidState);
        }
        let profile = self
            .provider
            .exchange_and_fetch_profile(code, &pending.code_verifier, redirect_uri)
            .await?;
        // Identity-stability: if THIS provider identity is
        // already linked to a user, that binding wins regardless
        // of what email the provider now reports. Defends against
        // an attacker who changes their provider-side email to
        // match an existing victim's address — the email-based
        // fallback would otherwise move the identity to them.
        let user = if let Some(user_id) = self
            .accounts
            .find_user_id_by_provider_identity(self.provider.name(), &profile.provider_user_id)
            .await?
        {
            self.lookup_user_or_internal(user_id).await?
        } else {
            self.link_or_create_user(&profile.email).await?
        };
        let (access_ct, access_nonce) = self
            .cipher
            .encrypt(profile.access_token.as_bytes())
            .map_err(|e| AuthError::Internal(format!("access-token encrypt failed: {e}")))?;
        let (refresh_ct, refresh_nonce) = match profile.refresh_token.as_deref() {
            Some(rt) => {
                let (ct, n) = self.cipher.encrypt(rt.as_bytes()).map_err(|e| {
                    AuthError::Internal(format!("refresh-token encrypt failed: {e}"))
                })?;
                (Some(ct), Some(n.to_vec()))
            }
            None => (None, None),
        };
        // A provider-supplied `expires_in` that doesn't fit in
        // `chrono::Duration` is malformed — silently coercing to
        // zero would mark the freshly-issued token as already
        // expired. Surface as `OAuthExchange` so the caller can
        // retry and ops can see the upstream bug.
        let expires_at = match profile.expires_in {
            Some(d) => Some(
                now + chrono::Duration::from_std(d).map_err(|e| {
                    AuthError::OAuthExchange(format!(
                        "provider returned expires_in outside chrono::Duration range: {e}"
                    ))
                })?,
            ),
            None => None,
        };
        self.accounts
            .upsert_oauth_account(NewOAuthAccount {
                user_id: user.id,
                provider: self.provider.name().to_owned(),
                provider_user_id: profile.provider_user_id,
                access_token_ciphertext: access_ct,
                access_token_nonce: access_nonce.to_vec(),
                refresh_token_ciphertext: refresh_ct,
                refresh_token_nonce: refresh_nonce,
                expires_at,
            })
            .await?;
        Ok(user)
    }

    /// Re-read a user by primary key, mapping `None` to
    /// `AuthError::Internal` since the caller just verified
    /// they existed.
    async fn lookup_user_or_internal(&self, user_id: Uuid) -> Result<AuthenticatedUser, AuthError> {
        let user = self.users.find_user_by_id(user_id).await?.ok_or_else(|| {
            AuthError::Internal(format!(
                "oauth_account references user {user_id} but the user row is gone"
            ))
        })?;
        Ok(AuthenticatedUser {
            id: user.id,
            email: user.email,
            email_verified_at: user.email_verified_at,
            created_at: user.created_at,
        })
    }

    /// Find a user by email or create a fresh one. The created
    /// user has `email_verified_at = now()` because the provider
    /// already attested the address.
    async fn link_or_create_user(&self, email: &str) -> Result<AuthenticatedUser, AuthError> {
        if let Some(user) = self.users.find_user_by_email(email).await? {
            // If the user registered locally and hasn't verified
            // yet, accept the provider's assertion as proof and
            // mark them verified. Idempotent on already-verified.
            let _ = self.users.mark_email_verified(user.id).await?;
            // Re-read to pick up the just-set timestamp.
            let refreshed = self.users.find_user_by_id(user.id).await?.ok_or_else(|| {
                AuthError::Internal(format!(
                    "user {user_id} disappeared mid-oauth-link",
                    user_id = user.id
                ))
            })?;
            return Ok(AuthenticatedUser {
                id: refreshed.id,
                email: refreshed.email,
                email_verified_at: refreshed.email_verified_at,
                created_at: refreshed.created_at,
            });
        }
        // The OAuth-created user has no password. The hash column
        // is non-null in the DB, so we generate a unique argon2id
        // PHC string from 32 bytes of `OsRng` and immediately
        // discard the plaintext — meaning even a future offline
        // crack of one row yields a useless one-off secret, not a
        // shared backdoor across every OAuth-only account.
        let user = match self
            .users
            .insert_user(email, &unguessable_password_hash().await?)
            .await
        {
            Ok(user) => user,
            Err(StorageError::UniqueViolation(_)) => {
                // Raced with another `link_or_create_user` for the
                // same email. Re-read.
                self.users.find_user_by_email(email).await?.ok_or_else(|| {
                    AuthError::Internal(
                        "race in link_or_create_user: insert failed but read returned None"
                            .to_owned(),
                    )
                })?
            }
            Err(e) => return Err(e.into()),
        };
        // Provider attested the address — mark verified.
        let _ = self.users.mark_email_verified(user.id).await?;
        let refreshed = self.users.find_user_by_id(user.id).await?.ok_or_else(|| {
            AuthError::Internal(format!(
                "user {user_id} disappeared mid-oauth-create",
                user_id = user.id
            ))
        })?;
        Ok(AuthenticatedUser {
            id: refreshed.id,
            email: refreshed.email,
            email_verified_at: refreshed.email_verified_at,
            created_at: refreshed.created_at,
        })
    }
}

/// Generate a unique argon2id PHC string whose plaintext is 32
/// bytes of `OsRng`-derived secret that's never returned to the
/// caller. Used to fill the non-null `password_hash` column on
/// OAuth-created users so a future offline crack of one row
/// yields only a one-off secret instead of a shared backdoor
/// across every OAuth-only account.
async fn unguessable_password_hash() -> Result<String, AuthError> {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    use rand::TryRngCore;
    use rand::rngs::OsRng;

    let mut bytes = [0u8; 32];
    OsRng
        .try_fill_bytes(&mut bytes)
        .expect("OsRng must provide entropy for OAuth password placeholder");
    // The plaintext is the random bytes encoded as base64. The
    // bytes themselves go out of scope at the end of the await;
    // only the resulting argon2id hash is ever returned.
    let plaintext = STANDARD_NO_PAD.encode(bytes);
    crate::password::hash_password(plaintext).await
}
