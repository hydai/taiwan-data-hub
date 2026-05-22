//! Google OAuth 2.1 + `OpenID` Connect provider (#4.4).
//!
//! Same `OAuthProvider` trait surface as the GitHub impl in
//! #4.3, but Google is OIDC: the token endpoint returns an
//! `id_token` (an RS256 JWT) alongside the access + refresh
//! tokens, and we extract the user identity straight from the
//! JWT's claims rather than calling a separate `/userinfo`
//! endpoint. This avoids a second HTTP round-trip per login and
//! defends against a network attacker who can MITM `/userinfo`
//! responses — only the JWT's RS256 signature, anchored on
//! Google's JWKS, is trusted.
//!
//! ## What we verify on every `id_token`
//!
//! 1. The header `kid` matches one of the JWKs Google advertises
//!    at <https://www.googleapis.com/oauth2/v3/certs>.
//! 2. The RS256 signature over `header.payload` verifies.
//! 3. `iss` is one of Google's two accepted issuer strings
//!    (`https://accounts.google.com` or `accounts.google.com`).
//! 4. `aud` equals our OAuth `client_id`.
//! 5. `exp` is in the future (with the small `leeway` the
//!    `jsonwebtoken` crate already provides).
//! 6. `email_verified` is `true` — an unverified address from
//!    Google would otherwise let an attacker register a user
//!    against a victim's domain.
//!
//! Anything else surfaces as `AuthError::OAuthExchange`. We
//! deliberately do NOT call Google's `/userinfo` endpoint as a
//! fallback; the JWT IS the user-identity record.
//!
//! ## JWKS caching
//!
//! [`JwksCache`] holds the last-fetched JWKS in a
//! `tokio::sync::Mutex` with a `last_fetched_at: Instant` so
//! repeated logins inside the TTL hit memory, and a cache miss
//! goes back to Google. We refetch on EVERY validation if the
//! cached entry is older than [`JWKS_TTL`]. Tests can hand in a
//! pre-populated cache via [`GoogleProvider::with_endpoints`] so
//! they don't have to mock the JWKS endpoint either.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use reqwest::Client;
use serde::Deserialize;
use tokio::sync::Mutex;
use url::Url;

use crate::error::AuthError;
use crate::oauth::provider::{OAuthProvider, ProviderProfile};

const REAL_AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const REAL_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const REAL_JWKS_URL: &str = "https://www.googleapis.com/oauth2/v3/certs";

/// Google publishes its OIDC discovery doc with both of these as
/// valid `iss` values. We accept either.
const ACCEPTED_ISSUERS: &[&str] = &["https://accounts.google.com", "accounts.google.com"];

/// Minimum OIDC scopes for "log in with Google": `openid` enables
/// the `id_token`, `email` puts the address + verified flag into
/// the JWT, `profile` is intentionally OMITTED — we don't store
/// the provider-supplied display name in v0.1.
const SCOPES: &str = "openid email";

/// JWKS cache TTL. Google rotates signing keys every few weeks
/// but advertises the new ones well in advance, so a generous
/// in-process TTL is fine — the cost of a missed rotation is one
/// extra failed login that re-fetches.
pub const JWKS_TTL: Duration = Duration::from_secs(60 * 60);

/// Production-shape Google OIDC provider.
#[derive(Clone)]
pub struct GoogleProvider {
    client_id: String,
    client_secret: String,
    http: Client,
    authorize_url: String,
    token_url: String,
    jwks: Arc<JwksCache>,
}

impl GoogleProvider {
    /// Production constructor: real Google endpoints, lazy JWKS
    /// cache (first login triggers the JWKS fetch).
    #[must_use]
    pub fn new(client_id: String, client_secret: String, http: Client) -> Self {
        Self {
            client_id,
            client_secret,
            http: http.clone(),
            authorize_url: REAL_AUTHORIZE_URL.to_owned(),
            token_url: REAL_TOKEN_URL.to_owned(),
            jwks: Arc::new(JwksCache::new(http, REAL_JWKS_URL.to_owned())),
        }
    }

    /// Test-only constructor that redirects every HTTP call at a
    /// local mock (wiremock) and lets the caller seed the JWKS
    /// cache with a pre-built decoding key so tests don't have to
    /// mock the JWKS endpoint as well.
    #[must_use]
    pub fn with_endpoints(
        client_id: String,
        client_secret: String,
        http: Client,
        authorize_url: String,
        token_url: String,
        jwks: Arc<JwksCache>,
    ) -> Self {
        Self {
            client_id,
            client_secret,
            http,
            authorize_url,
            token_url,
            jwks,
        }
    }
}

/// Raw shape of Google's `/token` response.
///
/// `id_token` is `Option` because the same struct deserialises
/// Google's OAuth error shape — the error-handling branch in
/// `exchange_token` rejects before we touch `id_token`.
#[derive(Debug, Deserialize)]
struct GoogleTokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// Validated token payload — populated by `exchange_token` after
/// it confirms the error branch is empty AND `access_token` +
/// `id_token` were both present.
struct ExchangedToken {
    access_token: String,
    id_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

/// Claims we read out of the validated `id_token`.
#[derive(Debug, Deserialize)]
struct IdTokenClaims {
    sub: String,
    email: String,
    #[serde(default)]
    email_verified: bool,
}

#[async_trait]
impl OAuthProvider for GoogleProvider {
    fn name(&self) -> &'static str {
        "google"
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
            .append_pair("response_type", "code")
            .append_pair("scope", SCOPES)
            .append_pair("state", state)
            .append_pair("code_challenge", code_challenge)
            .append_pair("code_challenge_method", "S256")
            // `access_type=offline` + `prompt=consent` is the
            // documented Google idiom for "give us a refresh
            // token even on re-authorize". Without `prompt`
            // Google omits the refresh_token on every login
            // after the first.
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent");
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
        let claims = self.validate_id_token(&token.id_token).await?;
        if !claims.email_verified {
            return Err(AuthError::OAuthExchange(
                "Google id_token reports email_verified=false".to_owned(),
            ));
        }
        Ok(ProviderProfile {
            provider_user_id: claims.sub,
            email: claims.email,
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            expires_in: token.expires_in.map(Duration::from_secs),
        })
    }
}

impl GoogleProvider {
    async fn exchange_token(
        &self,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> Result<ExchangedToken, AuthError> {
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
        // Read the body BEFORE branching on status so an HTTP 4xx
        // from Google still surfaces its OAuth-shaped error JSON
        // (`{"error": "invalid_grant", "error_description": "..."}`)
        // instead of being collapsed to "token endpoint returned
        // 400". The `error` branch below covers both the
        // 2xx-with-error-field and the 4xx-with-error-body cases.
        let status = resp.status();
        let body_text = resp.text().await.map_err(|e| {
            AuthError::OAuthExchange(format!("token body read failed: {e} (status={status})"))
        })?;
        let body: GoogleTokenResponse = match serde_json::from_str(&body_text) {
            Ok(body) => body,
            Err(e) => {
                // Non-JSON body: most informative thing we can do
                // is include the status + a short body snippet so
                // ops can diagnose. Truncate to keep the log line
                // bounded if Google ever responds with HTML.
                let snippet: String = body_text.chars().take(256).collect();
                return Err(AuthError::OAuthExchange(format!(
                    "token JSON decode failed: {e} (status={status}, body={snippet:?})"
                )));
            }
        };
        if let Some(err) = body.error.as_deref() {
            let msg = match body.error_description.as_deref() {
                Some(desc) if !desc.is_empty() => {
                    format!("Google rejected token exchange: {err} ({desc})")
                }
                _ => format!("Google rejected token exchange: {err}"),
            };
            return Err(AuthError::OAuthExchange(msg));
        }
        if !status.is_success() {
            // Non-success with no `error` field — unusual but
            // possible if Google returns a 5xx with an empty body
            // or a non-OAuth-shaped error. Surface the status +
            // body snippet so ops can debug.
            let snippet: String = body_text.chars().take(256).collect();
            return Err(AuthError::OAuthExchange(format!(
                "token endpoint returned {status} with no error field (body={snippet:?})"
            )));
        }
        if !matches!(body.token_type.as_deref(), Some("Bearer" | "bearer") | None) {
            return Err(AuthError::OAuthExchange(format!(
                "unexpected token_type: {:?}",
                body.token_type
            )));
        }
        let access_token = body.access_token.ok_or_else(|| {
            AuthError::OAuthExchange(
                "token endpoint returned 200 with no access_token and no error".to_owned(),
            )
        })?;
        let id_token = body.id_token.ok_or_else(|| {
            AuthError::OAuthExchange(
                "token endpoint returned 200 with no id_token (OIDC required)".to_owned(),
            )
        })?;
        Ok(ExchangedToken {
            access_token,
            id_token,
            refresh_token: body.refresh_token,
            expires_in: body.expires_in,
        })
    }

    async fn validate_id_token(&self, id_token: &str) -> Result<IdTokenClaims, AuthError> {
        // The header is decoded first (without verification) so we
        // know which `kid` to look up in the JWKS. The body's
        // signature is then verified against that JWK's RSA
        // components.
        let header = jsonwebtoken::decode_header(id_token)
            .map_err(|e| AuthError::OAuthExchange(format!("id_token header decode failed: {e}")))?;
        let kid = header
            .kid
            .ok_or_else(|| AuthError::OAuthExchange("id_token header missing kid".to_owned()))?;
        let key = self.jwks.decoding_key_for(&kid).await?;

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[&self.client_id]);
        validation.set_issuer(ACCEPTED_ISSUERS);
        validation.validate_exp = true;
        // `set_required_spec_claims` lists which standard claims
        // MUST be present. Without `exp` here the validator would
        // silently accept a JWT with no expiry, defeating the
        // point of `validate_exp = true`.
        validation.set_required_spec_claims(&["iss", "aud", "exp"]);

        let data = jsonwebtoken::decode::<IdTokenClaims>(id_token, &key, &validation)
            .map_err(|e| AuthError::OAuthExchange(format!("id_token verify failed: {e}")))?;
        Ok(data.claims)
    }
}

/// One JWK from Google's `/oauth2/v3/certs` JSON Web Key Set.
///
/// Only the fields we actually consume: the kid, RSA modulus +
/// exponent (base64url), and `alg` so we can refuse anything that
/// claims to be non-RS256 (defense in depth — we also pin
/// `Algorithm::RS256` in [`Validation`]).
#[derive(Debug, Clone, Deserialize)]
struct Jwk {
    kid: String,
    #[serde(default)]
    alg: Option<String>,
    n: String,
    e: String,
}

#[derive(Debug, Clone, Deserialize)]
struct JwkSet {
    keys: Vec<Jwk>,
}

/// Mutable JWKS cache shared across logins.
///
/// Two operating modes, distinguished by [`JwksCacheInner::frozen`]:
///
/// - **Production**: built with [`JwksCache::new`] — empty until
///   the first lookup, then fetched + refreshed every
///   [`JWKS_TTL`]. Also force-refreshed on a `kid` miss inside
///   the TTL window so Google can introduce new signing keys
///   without taking the service down for an hour.
/// - **Tests**: built with [`JwksCache::with_preseeded_keys`] —
///   the cache starts populated with caller-supplied keys and
///   marked `frozen`, so the refresh path is never reached and
///   the (deliberately empty) `jwks_url` is never dialled.
pub struct JwksCache {
    http: Client,
    jwks_url: String,
    inner: Mutex<JwksCacheInner>,
}

struct JwksCacheInner {
    keys: Vec<Jwk>,
    last_fetched_at: Option<Instant>,
    /// `true` when the cache was preseeded (tests). Refreshes are
    /// suppressed so a stale TTL or an unknown `kid` doesn't try
    /// to dial the empty `jwks_url`.
    frozen: bool,
}

impl JwksCache {
    /// Build an empty cache that fetches Google's JWKS lazily on
    /// first use.
    #[must_use]
    pub fn new(http: Client, jwks_url: String) -> Self {
        Self {
            http,
            jwks_url,
            inner: Mutex::new(JwksCacheInner {
                keys: Vec::new(),
                last_fetched_at: None,
                frozen: false,
            }),
        }
    }

    /// Test helper: pre-seed the cache with caller-supplied JWKs.
    /// The cache is marked `frozen` so lookups never trigger a
    /// refresh — the HTTP client + URL are unused and tests
    /// don't have to mock the JWKS endpoint.
    #[doc(hidden)]
    pub fn with_preseeded_keys(jwks_json: &str) -> Result<Arc<Self>, AuthError> {
        let JwkSet { keys } = serde_json::from_str(jwks_json)
            .map_err(|e| AuthError::OAuthExchange(format!("preseeded JWKS not valid JSON: {e}")))?;
        Ok(Arc::new(Self {
            // Both fields are unreachable while `frozen = true`;
            // they exist only to satisfy the struct shape.
            http: Client::new(),
            jwks_url: String::new(),
            inner: Mutex::new(JwksCacheInner {
                keys,
                last_fetched_at: Some(Instant::now()),
                frozen: true,
            }),
        }))
    }

    async fn decoding_key_for(&self, kid: &str) -> Result<DecodingKey, AuthError> {
        // The double-check pattern: take the lock, look at the
        // age, and refresh INSIDE the lock so concurrent callers
        // don't all stampede the JWKS endpoint.
        let mut guard = self.inner.lock().await;
        let stale = !guard.frozen
            && guard
                .last_fetched_at
                .is_none_or(|t| t.elapsed() >= JWKS_TTL);
        if stale {
            let fetched = self.fetch_jwks().await?;
            guard.keys = fetched.keys;
            guard.last_fetched_at = Some(Instant::now());
        }
        // Resilient kid-miss path: if Google rotates a signing key
        // inside the TTL window the cache won't have it yet, so a
        // miss is allowed to force ONE refresh + retry (still under
        // the mutex so concurrent callers don't stampede). Without
        // this every login fails for up to JWKS_TTL after a Google
        // key rotation. A frozen test cache skips this branch.
        let mut jwk = guard.keys.iter().find(|k| k.kid == kid).cloned();
        if jwk.is_none() && !guard.frozen && !stale {
            let fetched = self.fetch_jwks().await?;
            guard.keys = fetched.keys;
            guard.last_fetched_at = Some(Instant::now());
            jwk = guard.keys.iter().find(|k| k.kid == kid).cloned();
        }
        let jwk = jwk.ok_or_else(|| {
            AuthError::OAuthExchange(format!("no JWK matches kid={kid} in Google's JWKS"))
        })?;
        if let Some(alg) = jwk.alg.as_deref()
            && alg != "RS256"
        {
            return Err(AuthError::OAuthExchange(format!(
                "JWK kid={kid} advertises alg={alg}, refusing (only RS256 accepted)"
            )));
        }
        DecodingKey::from_rsa_components(&jwk.n, &jwk.e).map_err(|e| {
            AuthError::OAuthExchange(format!("JWK kid={kid} not valid RSA components: {e}"))
        })
    }

    async fn fetch_jwks(&self) -> Result<JwkSet, AuthError> {
        let resp = self
            .http
            .get(&self.jwks_url)
            .send()
            .await
            .map_err(|e| AuthError::OAuthExchange(format!("JWKS GET failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(AuthError::OAuthExchange(format!(
                "JWKS endpoint returned {}",
                resp.status()
            )));
        }
        resp.json::<JwkSet>()
            .await
            .map_err(|e| AuthError::OAuthExchange(format!("JWKS JSON decode failed: {e}")))
    }
}
