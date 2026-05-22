//! Provider trait shared by `github` (#4.3) and `google` (#4.4).
//!
//! Two responsibilities:
//!
//! 1. Build the authorize-redirect URL clients are sent to.
//! 2. Exchange the authorization `code` for an access token,
//!    then fetch enough profile to link to a `users` row.
//!
//! Implementations are stateless — they hold the client-id /
//! client-secret + a `reqwest::Client` for the HTTP round trips.

use async_trait::async_trait;
use url::Url;

use crate::error::AuthError;

/// Provider-side identity attached to the OAuth account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderProfile {
    /// Stable provider-side id (GitHub `user.id` as decimal
    /// string, Google `sub`). Used as the (`provider`,
    /// `provider_user_id`) primary key on `oauth_accounts`.
    pub provider_user_id: String,
    /// Verified email address (the auth service refuses to
    /// link unverified emails so a hostile provider can't
    /// hijack an existing user by squatting an unverified
    /// address that happens to match).
    pub email: String,
    /// Provider-issued access token. Will be AES-GCM-encrypted
    /// before storage.
    pub access_token: String,
    /// Optional refresh token. GitHub OAuth Apps return None;
    /// Google + GitHub Apps with `expires_in` do return one.
    pub refresh_token: Option<String>,
    /// Optional access-token TTL.
    pub expires_in: Option<std::time::Duration>,
}

#[async_trait]
pub trait OAuthProvider: Send + Sync {
    /// Stable wire identifier (`"github"`, `"google"`). Matches
    /// the `provider` column / CHECK constraint.
    fn name(&self) -> &'static str;

    /// Build the authorize URL the user is redirected to.
    /// `state` is the cleartext CSRF token, `code_challenge` is
    /// the PKCE S256 challenge, `redirect_uri` is the callback.
    fn authorize_url(
        &self,
        state: &str,
        code_challenge: &str,
        redirect_uri: &str,
    ) -> Result<Url, AuthError>;

    /// Exchange the authorization `code` for an access token and
    /// fetch the user profile in one call (providers differ in
    /// how many HTTP round-trips that takes; the trait hides it).
    /// `code_verifier` is the PKCE secret we generated at
    /// `start_login` time.
    async fn exchange_and_fetch_profile(
        &self,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> Result<ProviderProfile, AuthError>;
}
