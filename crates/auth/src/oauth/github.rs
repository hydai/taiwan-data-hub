//! GitHub OAuth provider (#4.3).
//!
//! Implements the `OAuthProvider` trait against GitHub's
//! OAuth 2.1 endpoints:
//!
//! - Authorize:   `https://github.com/login/oauth/authorize`
//! - Token:       `https://github.com/login/oauth/access_token`
//! - User:        `https://api.github.com/user`
//! - Emails:      `https://api.github.com/user/emails`
//!
//! The `*_base_url` fields are split so wiremock-backed tests can
//! point them at a local mock server. Production callers use
//! [`GitHubProvider::new`] which hardcodes the real GitHub URLs.

use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use url::Url;

use crate::error::AuthError;
use crate::oauth::provider::{OAuthProvider, ProviderProfile};

const REAL_AUTHORIZE_URL: &str = "https://github.com/login/oauth/authorize";
const REAL_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const REAL_API_BASE: &str = "https://api.github.com";

const READ_SCOPES: &str = "read:user user:email";

/// Stable GitHub OAuth provider.
#[derive(Clone)]
pub struct GitHubProvider {
    client_id: String,
    client_secret: String,
    http: Client,
    authorize_url: String,
    token_url: String,
    api_base: String,
}

impl GitHubProvider {
    /// Production constructor: real GitHub URLs.
    #[must_use]
    pub fn new(client_id: String, client_secret: String, http: Client) -> Self {
        Self {
            client_id,
            client_secret,
            http,
            authorize_url: REAL_AUTHORIZE_URL.to_owned(),
            token_url: REAL_TOKEN_URL.to_owned(),
            api_base: REAL_API_BASE.to_owned(),
        }
    }

    /// Test-only constructor that lets the caller redirect every
    /// HTTP call at a local mock (wiremock).
    #[must_use]
    pub fn with_endpoints(
        client_id: String,
        client_secret: String,
        http: Client,
        authorize_url: String,
        token_url: String,
        api_base: String,
    ) -> Self {
        Self {
            client_id,
            client_secret,
            http,
            authorize_url,
            token_url,
            api_base,
        }
    }
}

#[derive(Debug, Deserialize)]
struct GitHubTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    token_type: Option<String>,
    /// Present in the error shape. The success shape never sets
    /// it, so a populated `error` overrides `access_token` even
    /// if the response is otherwise ambiguous.
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubUser {
    // The /user endpoint includes an `email` field too but it
    // can be the user's "public" email or null. We always read
    // the verified primary from `/user/emails` instead, so `id`
    // is the only field this struct deserialises.
    id: u64,
}

#[derive(Debug, Deserialize)]
struct GitHubEmail {
    email: String,
    primary: bool,
    verified: bool,
}

#[async_trait]
impl OAuthProvider for GitHubProvider {
    fn name(&self) -> &'static str {
        "github"
    }

    fn authorize_url(
        &self,
        state: &str,
        code_challenge: &str,
        redirect_uri: &str,
    ) -> Result<Url, AuthError> {
        let mut url = Url::parse(&self.authorize_url)
            .map_err(|e| AuthError::InvalidConfig(format!("authorize_url is not a URL: {e}")))?;
        url.query_pairs_mut()
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", READ_SCOPES)
            .append_pair("state", state)
            .append_pair("code_challenge", code_challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("response_type", "code");
        Ok(url)
    }

    async fn exchange_and_fetch_profile(
        &self,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> Result<ProviderProfile, AuthError> {
        let token = self
            .exchange_token(code, code_verifier, redirect_uri)
            .await?;
        let email = self
            .fetch_primary_verified_email(&token.access_token)
            .await?;
        let user = self.fetch_user(&token.access_token).await?;
        Ok(ProviderProfile {
            provider_user_id: user.id.to_string(),
            email,
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            expires_in: token.expires_in.map(std::time::Duration::from_secs),
        })
    }
}

impl GitHubProvider {
    async fn exchange_token(
        &self,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> Result<GitHubTokenResponse, AuthError> {
        let resp = self
            .http
            .post(&self.token_url)
            .header("Accept", "application/json")
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("code", code),
                ("code_verifier", code_verifier),
                ("redirect_uri", redirect_uri),
                ("grant_type", "authorization_code"),
            ])
            .send()
            .await
            .map_err(|e| AuthError::OAuthExchange(format!("token POST failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(AuthError::OAuthExchange(format!(
                "token endpoint returned {}",
                resp.status()
            )));
        }
        let body: GitHubTokenResponse = resp
            .json()
            .await
            .map_err(|e| AuthError::OAuthExchange(format!("token JSON decode failed: {e}")))?;
        if let Some(err) = body.error.as_deref() {
            return Err(AuthError::OAuthExchange(format!(
                "GitHub rejected token exchange: {err} ({})",
                body.error_description.as_deref().unwrap_or(""),
            )));
        }
        if !matches!(body.token_type.as_deref(), Some("bearer" | "Bearer") | None) {
            return Err(AuthError::OAuthExchange(format!(
                "unexpected token_type: {:?}",
                body.token_type
            )));
        }
        Ok(body)
    }

    async fn fetch_user(&self, access_token: &str) -> Result<GitHubUser, AuthError> {
        let url = format!("{}/user", self.api_base);
        let resp = self
            .http
            .get(url)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "taiwan-data-hub")
            .send()
            .await
            .map_err(|e| AuthError::OAuthExchange(format!("/user GET failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(AuthError::OAuthExchange(format!(
                "/user returned {}",
                resp.status()
            )));
        }
        resp.json()
            .await
            .map_err(|e| AuthError::OAuthExchange(format!("/user JSON decode failed: {e}")))
    }

    async fn fetch_primary_verified_email(&self, access_token: &str) -> Result<String, AuthError> {
        let url = format!("{}/user/emails", self.api_base);
        let resp = self
            .http
            .get(url)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "taiwan-data-hub")
            .send()
            .await
            .map_err(|e| AuthError::OAuthExchange(format!("/user/emails GET failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(AuthError::OAuthExchange(format!(
                "/user/emails returned {}",
                resp.status()
            )));
        }
        let emails: Vec<GitHubEmail> = resp
            .json()
            .await
            .map_err(|e| AuthError::OAuthExchange(format!("/user/emails decode failed: {e}")))?;
        emails
            .into_iter()
            .find(|e| e.primary && e.verified)
            .map(|e| e.email)
            .ok_or_else(|| {
                AuthError::OAuthExchange("GitHub account has no primary verified email".to_owned())
            })
    }
}
