//! End-to-end tests for `auth::OAuthService<GoogleProvider>`.
//!
//! Same shape as `tests/oauth.rs` (the GitHub flow), but #4.4
//! requires testing the OIDC `id_token` verification path. The
//! fixture:
//!
//! 1. Generates an RSA-2048 keypair once per process (via
//!    `OnceLock`) so individual tests don't repeatedly pay the
//!    ~1s key-gen cost.
//! 2. Builds a Google-shaped JWKS JSON from the public modulus +
//!    exponent and pre-seeds the [`JwksCache`] with it so tests
//!    never need a wiremock mock for the JWKS endpoint.
//! 3. Signs a Google-shaped `id_token` JWT (`iss`, `aud`, `sub`,
//!    `email`, `email_verified`, `exp`) with the private key and
//!    `kid = "test-key"` so the cached JWK matches.
//!
//! No real Postgres, no real Google.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use auth::{AuthError, GoogleProvider, JwksCache, OAuthService, TokenCipher, account_aad};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use rand_core::OsRng;
use reqwest::Client;
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde::Serialize;
use serde_json::json;
use storage::{
    NewOAuthAccount, OAuthAccountRepo, OAuthPendingState, OAuthStateRepo, StorageError, User,
    UserRepo,
};
use url::Url;
use uuid::Uuid;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// --- in-memory fakes -------------------------------------------------
//
// These mirror the production sqlx-backed repos so the auth crate's
// invariants are exercised end-to-end without a real Postgres.

#[derive(Default)]
struct InMemoryUserRepo {
    inner: Mutex<HashMap<Uuid, User>>,
}

#[async_trait]
impl UserRepo for InMemoryUserRepo {
    async fn insert_user(&self, email: &str, password_hash: &str) -> Result<User, StorageError> {
        let mut inner = self.inner.lock().unwrap();
        if inner
            .values()
            .any(|u| u.email.to_lowercase().eq(&email.to_lowercase()))
        {
            return Err(StorageError::UniqueViolation("users_email_key".to_owned()));
        }
        let now = Utc::now();
        let user = User {
            id: Uuid::now_v7(),
            email: email.to_owned(),
            password_hash: password_hash.to_owned(),
            email_verified_at: None,
            created_at: now,
            updated_at: now,
        };
        inner.insert(user.id, user.clone());
        Ok(user)
    }
    async fn find_user_by_email(&self, email: &str) -> Result<Option<User>, StorageError> {
        let inner = self.inner.lock().unwrap();
        Ok(inner
            .values()
            .find(|u| u.email.to_lowercase().eq(&email.to_lowercase()))
            .cloned())
    }
    async fn find_user_by_id(&self, id: Uuid) -> Result<Option<User>, StorageError> {
        Ok(self.inner.lock().unwrap().get(&id).cloned())
    }
    async fn mark_email_verified(&self, id: Uuid) -> Result<bool, StorageError> {
        let mut inner = self.inner.lock().unwrap();
        let Some(user) = inner.get_mut(&id) else {
            return Ok(false);
        };
        if user.email_verified_at.is_some() {
            return Ok(false);
        }
        let now = Utc::now();
        user.email_verified_at = Some(now);
        user.updated_at = now;
        Ok(true)
    }
    async fn update_password_hash(&self, id: Uuid, h: &str) -> Result<bool, StorageError> {
        let mut inner = self.inner.lock().unwrap();
        let Some(user) = inner.get_mut(&id) else {
            return Ok(false);
        };
        h.clone_into(&mut user.password_hash);
        user.updated_at = Utc::now();
        Ok(true)
    }
    async fn delete_user(&self, id: Uuid) -> Result<bool, StorageError> {
        Ok(self.inner.lock().unwrap().remove(&id).is_some())
    }
}

#[derive(Default)]
struct InMemoryOAuthStateRepo {
    inner: Mutex<HashMap<Vec<u8>, OAuthPendingState>>,
}
#[async_trait]
impl OAuthStateRepo for InMemoryOAuthStateRepo {
    async fn insert_oauth_state(&self, pending: OAuthPendingState) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().unwrap();
        if inner.contains_key(&pending.state_hash) {
            return Err(StorageError::UniqueViolation(
                "oauth_states_pkey".to_owned(),
            ));
        }
        inner.insert(pending.state_hash.clone(), pending);
        Ok(())
    }
    async fn consume_oauth_state(
        &self,
        state_hash: &[u8],
        provider: &str,
        redirect_uri: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<OAuthPendingState>, StorageError> {
        let mut inner = self.inner.lock().unwrap();
        let matches = inner.get(state_hash).is_some_and(|row| {
            row.provider == provider && row.redirect_uri == redirect_uri && row.expires_at > now
        });
        if !matches {
            return Ok(None);
        }
        Ok(inner.remove(state_hash))
    }
}

#[derive(Default)]
struct InMemoryOAuthAccountRepo {
    inner: Mutex<HashMap<(String, String), NewOAuthAccount>>,
}
#[async_trait]
impl OAuthAccountRepo for InMemoryOAuthAccountRepo {
    async fn upsert_oauth_account(&self, new: NewOAuthAccount) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().unwrap();
        let key = (new.provider.clone(), new.provider_user_id.clone());
        if let Some(existing) = inner.get_mut(&key) {
            existing.access_token_ciphertext = new.access_token_ciphertext;
            existing.access_token_nonce = new.access_token_nonce;
            existing.refresh_token_ciphertext = new.refresh_token_ciphertext;
            existing.refresh_token_nonce = new.refresh_token_nonce;
            existing.expires_at = new.expires_at;
        } else {
            let user_provider_collision = inner
                .values()
                .any(|row| row.user_id == new.user_id && row.provider == new.provider);
            if user_provider_collision {
                return Err(StorageError::UniqueViolation(
                    "oauth_accounts_user_id_provider_key".to_owned(),
                ));
            }
            inner.insert(key, new);
        }
        Ok(())
    }

    async fn find_user_id_by_provider_identity(
        &self,
        provider: &str,
        provider_user_id: &str,
    ) -> Result<Option<Uuid>, StorageError> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .get(&(provider.to_owned(), provider_user_id.to_owned()))
            .map(|row| row.user_id))
    }
}

// --- JWT signing fixture --------------------------------------------

/// One RSA-2048 keypair shared by every test in this file. Lazy +
/// process-wide so the ~1s key-gen cost is paid once.
fn test_signing_key() -> &'static RsaPrivateKey {
    static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
    KEY.get_or_init(|| {
        // `rand_core::OsRng` lines up with `rsa` 0.9's expected
        // `CryptoRngCore` trait (rand_core 0.6.x), separate from
        // the `rand` 0.9 we use in production paths.
        let mut rng = OsRng;
        RsaPrivateKey::new(&mut rng, 2048).expect("RSA-2048 keygen for tests")
    })
}

/// JWKS JSON the production code parses into the cache. `kid`
/// must match what we set in the JWT header.
fn test_jwks_json() -> String {
    let pub_key = RsaPublicKey::from(test_signing_key());
    let n = URL_SAFE_NO_PAD.encode(pub_key.n().to_bytes_be());
    let e = URL_SAFE_NO_PAD.encode(pub_key.e().to_bytes_be());
    json!({
        "keys": [
            { "kid": "test-key", "kty": "RSA", "alg": "RS256", "use": "sig", "n": n, "e": e }
        ]
    })
    .to_string()
}

fn jwks_cache() -> Arc<JwksCache> {
    JwksCache::with_preseeded_keys(&test_jwks_json()).expect("preseed JWKS")
}

#[derive(Serialize)]
struct IdTokenClaims<'a> {
    iss: &'a str,
    aud: &'a str,
    sub: &'a str,
    email: &'a str,
    email_verified: bool,
    iat: i64,
    exp: i64,
}

/// PKCS#1 DER bytes of the shared test key. `jsonwebtoken` with
/// `default-features = false` exposes `from_rsa_der` but not
/// `from_rsa_pem`; DER avoids enabling the extra feature.
fn test_signing_der() -> Vec<u8> {
    test_signing_key()
        .to_pkcs1_der()
        .expect("PKCS#1 DER")
        .as_bytes()
        .to_vec()
}

/// Sign a Google-shaped `id_token` with the shared test RSA key
/// and `kid = "test-key"` so the JWKS lookup finds it.
fn sign_id_token(claims: &IdTokenClaims<'_>) -> String {
    let key = EncodingKey::from_rsa_der(&test_signing_der());
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test-key".to_owned());
    jsonwebtoken::encode(&header, claims, &key).expect("sign id_token")
}

fn google_claims<'a>(
    sub: &'a str,
    email: &'a str,
    email_verified: bool,
    aud: &'a str,
) -> IdTokenClaims<'a> {
    let now = Utc::now().timestamp();
    IdTokenClaims {
        iss: "https://accounts.google.com",
        aud,
        sub,
        email,
        email_verified,
        iat: now,
        exp: now + 3600,
    }
}

// --- service wiring -------------------------------------------------

fn test_kek() -> [u8; 32] {
    [42u8; 32]
}

fn build_service(
    token_url_base: &str,
) -> (
    OAuthService<GoogleProvider>,
    Arc<InMemoryUserRepo>,
    Arc<InMemoryOAuthAccountRepo>,
    Arc<InMemoryOAuthStateRepo>,
) {
    let users: Arc<InMemoryUserRepo> = Arc::new(InMemoryUserRepo::default());
    let states: Arc<InMemoryOAuthStateRepo> = Arc::new(InMemoryOAuthStateRepo::default());
    let accounts: Arc<InMemoryOAuthAccountRepo> = Arc::new(InMemoryOAuthAccountRepo::default());
    let provider = GoogleProvider::with_endpoints(
        "test-client-id".to_owned(),
        "test-client-secret".to_owned(),
        Client::new(),
        format!("{token_url_base}/o/oauth2/v2/auth"),
        format!("{token_url_base}/token"),
        jwks_cache(),
    );
    let svc = OAuthService::new(
        provider,
        states.clone() as Arc<dyn OAuthStateRepo>,
        accounts.clone() as Arc<dyn OAuthAccountRepo>,
        users.clone() as Arc<dyn UserRepo>,
        TokenCipher::new(&test_kek()).unwrap(),
    );
    (svc, users, accounts, states)
}

async fn install_google_token_mock(server: &MockServer, id_token: &str) {
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code_verifier="))
        .and(body_string_contains("redirect_uri="))
        .and(body_string_contains("client_id=test-client-id"))
        .and(body_string_contains("client_secret=test-client-secret"))
        .and(body_string_contains("code="))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "ya29.test-access-token",
            "token_type": "Bearer",
            "id_token": id_token,
            "refresh_token": "1//test-refresh-token",
            "expires_in": 3599
        })))
        .mount(server)
        .await;
}

fn token_from_url(url: &Url, key: &str) -> Option<String> {
    url.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

// --- tests ----------------------------------------------------------

#[tokio::test]
async fn start_login_returns_google_authorize_url_with_oidc_params() {
    let server = MockServer::start().await;
    let (svc, _, _, _) = build_service(&server.uri());
    let started = svc
        .start_login("https://hub.example/oauth/callback")
        .await
        .unwrap();
    let url = started.redirect_to;
    assert!(url.as_str().contains("/o/oauth2/v2/auth"));
    assert_eq!(
        token_from_url(&url, "client_id").as_deref(),
        Some("test-client-id")
    );
    assert_eq!(
        token_from_url(&url, "response_type").as_deref(),
        Some("code")
    );
    assert_eq!(
        token_from_url(&url, "scope").as_deref(),
        Some("openid email"),
        "scope must request OIDC id_token + email"
    );
    assert_eq!(
        token_from_url(&url, "code_challenge_method").as_deref(),
        Some("S256")
    );
    assert_eq!(
        token_from_url(&url, "access_type").as_deref(),
        Some("offline"),
        "access_type=offline is Google's documented refresh-token gate"
    );
    assert_eq!(
        token_from_url(&url, "prompt").as_deref(),
        Some("consent"),
        "prompt=consent forces Google to re-issue refresh_token on every login"
    );
    let challenge = token_from_url(&url, "code_challenge").unwrap();
    assert_eq!(challenge.len(), 43, "PKCE S256 challenge is 43 chars");
}

#[tokio::test]
async fn finish_login_creates_user_and_stores_encrypted_tokens() {
    let server = MockServer::start().await;
    let id_token = sign_id_token(&google_claims(
        "google-sub-12345",
        "alice@example.com",
        true,
        "test-client-id",
    ));
    install_google_token_mock(&server, &id_token).await;
    let (svc, users, accounts, _) = build_service(&server.uri());

    let started = svc.start_login("https://hub.example/cb").await.unwrap();
    let state = token_from_url(&started.redirect_to, "state").unwrap();
    let authed = svc
        .finish_login("test-code", &state, "https://hub.example/cb")
        .await
        .unwrap();

    assert_eq!(authed.email, "alice@example.com");
    assert!(authed.email_verified_at.is_some());

    assert!(
        users
            .find_user_by_email("alice@example.com")
            .await
            .unwrap()
            .is_some()
    );

    let account = accounts
        .inner
        .lock()
        .unwrap()
        .get(&("google".to_owned(), "google-sub-12345".to_owned()))
        .cloned()
        .expect("oauth_accounts row for google sub");
    assert!(!account.access_token_ciphertext.is_empty());
    assert_eq!(account.access_token_nonce.len(), 12);
    assert!(
        account.refresh_token_ciphertext.is_some(),
        "Google returned a refresh_token; we should have stored it"
    );
    assert_eq!(account.refresh_token_nonce.as_ref().unwrap().len(), 12);
    // Cleartext token must not leak into the ciphertext bytes.
    assert!(
        !account
            .access_token_ciphertext
            .windows(b"ya29.test".len())
            .any(|w| w == b"ya29.test"),
        "raw Google access token must not appear in ciphertext"
    );
}

#[tokio::test]
async fn finish_login_links_to_existing_user_by_email() {
    let server = MockServer::start().await;
    let id_token = sign_id_token(&google_claims(
        "google-sub-67890",
        "bob@example.com",
        true,
        "test-client-id",
    ));
    install_google_token_mock(&server, &id_token).await;
    let (svc, users, accounts, _) = build_service(&server.uri());

    let preexisting = users
        .insert_user(
            "bob@example.com",
            "$argon2id$v=19$m=19456,t=2,p=1$Zm9v$Zm9v",
        )
        .await
        .unwrap();

    let started = svc.start_login("https://hub.example/cb").await.unwrap();
    let state = token_from_url(&started.redirect_to, "state").unwrap();
    let authed = svc
        .finish_login("test-code", &state, "https://hub.example/cb")
        .await
        .unwrap();

    assert_eq!(authed.id, preexisting.id, "must link, not create");
    assert!(authed.email_verified_at.is_some());
    assert!(
        accounts
            .inner
            .lock()
            .unwrap()
            .contains_key(&("google".to_owned(), "google-sub-67890".to_owned()))
    );
}

#[tokio::test]
async fn finish_login_rejects_unverified_email() {
    let server = MockServer::start().await;
    let id_token = sign_id_token(&google_claims(
        "google-sub-1",
        "carol@example.com",
        // The whole point of OIDC `email_verified=false` is that
        // Google does NOT vouch for this address. Linking it to a
        // local user would be an account-takeover vector.
        false,
        "test-client-id",
    ));
    install_google_token_mock(&server, &id_token).await;
    let (svc, _, _, _) = build_service(&server.uri());

    let started = svc.start_login("https://hub.example/cb").await.unwrap();
    let state = token_from_url(&started.redirect_to, "state").unwrap();
    let err = svc
        .finish_login("test-code", &state, "https://hub.example/cb")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, AuthError::OAuthExchange(msg) if msg.contains("email_verified=false")),
        "got: {err:?}"
    );
}

#[tokio::test]
async fn finish_login_rejects_wrong_audience() {
    let server = MockServer::start().await;
    // Sign with `aud` = a DIFFERENT client_id (e.g. a token that
    // was minted for some other application — an attacker
    // forwarding a token they got from a different OIDC flow).
    let id_token = sign_id_token(&google_claims(
        "google-sub-2",
        "dan@example.com",
        true,
        "some-other-client-id",
    ));
    install_google_token_mock(&server, &id_token).await;
    let (svc, _, _, _) = build_service(&server.uri());

    let started = svc.start_login("https://hub.example/cb").await.unwrap();
    let state = token_from_url(&started.redirect_to, "state").unwrap();
    let err = svc
        .finish_login("test-code", &state, "https://hub.example/cb")
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::OAuthExchange(_)), "got: {err:?}");
}

#[tokio::test]
async fn finish_login_rejects_wrong_issuer() {
    let server = MockServer::start().await;
    let bad_claims = IdTokenClaims {
        iss: "https://evil.example",
        aud: "test-client-id",
        sub: "google-sub-3",
        email: "eve@example.com",
        email_verified: true,
        iat: Utc::now().timestamp(),
        exp: Utc::now().timestamp() + 3600,
    };
    let id_token = sign_id_token(&bad_claims);
    install_google_token_mock(&server, &id_token).await;
    let (svc, _, _, _) = build_service(&server.uri());

    let started = svc.start_login("https://hub.example/cb").await.unwrap();
    let state = token_from_url(&started.redirect_to, "state").unwrap();
    let err = svc
        .finish_login("test-code", &state, "https://hub.example/cb")
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::OAuthExchange(_)));
}

#[tokio::test]
async fn finish_login_rejects_expired_id_token() {
    let server = MockServer::start().await;
    let now = Utc::now().timestamp();
    let id_token = sign_id_token(&IdTokenClaims {
        iss: "https://accounts.google.com",
        aud: "test-client-id",
        sub: "google-sub-4",
        email: "frank@example.com",
        email_verified: true,
        iat: now - 7200,
        exp: now - 3600,
    });
    install_google_token_mock(&server, &id_token).await;
    let (svc, _, _, _) = build_service(&server.uri());

    let started = svc.start_login("https://hub.example/cb").await.unwrap();
    let state = token_from_url(&started.redirect_to, "state").unwrap();
    let err = svc
        .finish_login("test-code", &state, "https://hub.example/cb")
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::OAuthExchange(_)));
}

#[tokio::test]
async fn finish_login_rejects_id_token_with_unknown_kid() {
    let server = MockServer::start().await;
    // Sign with a kid that does NOT exist in the seeded JWKS.
    // The decode path must surface this as OAuthExchange — not
    // silently fall back to /userinfo or accept the token.
    let key = EncodingKey::from_rsa_der(&test_signing_der());
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("attacker-key".to_owned());
    let claims = google_claims("google-sub-5", "g@example.com", true, "test-client-id");
    let id_token = jsonwebtoken::encode(&header, &claims, &key).expect("sign");
    install_google_token_mock(&server, &id_token).await;
    let (svc, _, _, _) = build_service(&server.uri());

    let started = svc.start_login("https://hub.example/cb").await.unwrap();
    let state = token_from_url(&started.redirect_to, "state").unwrap();
    let err = svc
        .finish_login("test-code", &state, "https://hub.example/cb")
        .await
        .unwrap_err();
    assert!(matches!(&err, AuthError::OAuthExchange(msg) if msg.contains("kid")));
}

#[tokio::test]
async fn finish_login_provider_identity_stays_bound_to_original_user() {
    let server = MockServer::start().await;
    let id_token = sign_id_token(&google_claims(
        "google-sub-9999",
        "alice@example.com",
        true,
        "test-client-id",
    ));
    install_google_token_mock(&server, &id_token).await;
    let (svc, users, accounts, _) = build_service(&server.uri());

    let started = svc.start_login("https://hub.example/cb").await.unwrap();
    let state = token_from_url(&started.redirect_to, "state").unwrap();
    let alice = svc
        .finish_login("c1", &state, "https://hub.example/cb")
        .await
        .unwrap();

    // Rename Alice's local row and create a second user with the
    // original email — simulating an attacker who controls a
    // Google account that happens to claim the same address.
    {
        let mut inner = users.inner.lock().unwrap();
        let row = inner.get_mut(&alice.id).unwrap();
        row.email = "alice-renamed@example.com".to_owned();
    }
    let snitch_id = users
        .insert_user(
            "alice@example.com",
            "$argon2id$v=19$m=19456,t=2,p=1$Zm9v$Zm9v",
        )
        .await
        .unwrap()
        .id;

    let started = svc.start_login("https://hub.example/cb").await.unwrap();
    let state = token_from_url(&started.redirect_to, "state").unwrap();
    let alice_again = svc
        .finish_login("c2", &state, "https://hub.example/cb")
        .await
        .unwrap();
    assert_eq!(alice_again.id, alice.id, "must remain bound to original");
    assert_ne!(alice_again.id, snitch_id);
    let row = accounts
        .inner
        .lock()
        .unwrap()
        .get(&("google".to_owned(), "google-sub-9999".to_owned()))
        .cloned()
        .unwrap();
    assert_eq!(row.user_id, alice.id);
}

#[tokio::test]
async fn finish_login_persists_refresh_token_when_rotated() {
    // Google rotates refresh_token semantics: on a subsequent
    // login (with `prompt=consent`) a fresh refresh_token is
    // issued. The repo upsert must write the NEW token, not
    // silently keep the original. We mock two distinct token
    // responses and verify the second one's refresh_token lands
    // in the row.
    let server = MockServer::start().await;

    // Server-side counter for the canned response. Each /token
    // call returns a different refresh_token and the SAME `sub`
    // (so identity stays bound) — the auth crate's upsert path
    // is what we're verifying.
    let id_token = sign_id_token(&google_claims(
        "google-sub-rot",
        "rot@example.com",
        true,
        "test-client-id",
    ));
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "ya29.first",
            "token_type": "Bearer",
            "id_token": id_token,
            "refresh_token": "1//first-refresh",
            "expires_in": 3599
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "ya29.second",
            "token_type": "Bearer",
            "id_token": id_token,
            "refresh_token": "1//second-refresh",
            "expires_in": 3599
        })))
        .mount(&server)
        .await;

    let (svc, _, accounts, _) = build_service(&server.uri());
    let started = svc.start_login("https://hub.example/cb").await.unwrap();
    let s1 = token_from_url(&started.redirect_to, "state").unwrap();
    svc.finish_login("c1", &s1, "https://hub.example/cb")
        .await
        .unwrap();
    let (first_ct, first_nonce) = {
        let row = accounts
            .inner
            .lock()
            .unwrap()
            .get(&("google".to_owned(), "google-sub-rot".to_owned()))
            .cloned()
            .expect("first row");
        (
            row.refresh_token_ciphertext.expect("first refresh CT"),
            row.refresh_token_nonce.expect("first refresh nonce"),
        )
    };

    // Second authorization with the SAME google sub but a new
    // refresh token (Google's documented "prompt=consent" path).
    let started = svc.start_login("https://hub.example/cb").await.unwrap();
    let s2 = token_from_url(&started.redirect_to, "state").unwrap();
    svc.finish_login("c2", &s2, "https://hub.example/cb")
        .await
        .unwrap();
    let (second_ct, second_nonce) = {
        let row = accounts
            .inner
            .lock()
            .unwrap()
            .get(&("google".to_owned(), "google-sub-rot".to_owned()))
            .cloned()
            .expect("second row");
        (
            row.refresh_token_ciphertext.expect("second refresh CT"),
            row.refresh_token_nonce.expect("second refresh nonce"),
        )
    };

    // Comparing ciphertexts alone is meaningless — AES-GCM uses a
    // fresh random nonce per encrypt, so two encryptions of the
    // SAME plaintext also differ. Decrypt with the AAD bound to
    // the row identity and compare plaintexts instead.
    let cipher = TokenCipher::new(&test_kek()).unwrap();
    let aad = account_aad("google", "google-sub-rot");
    let first_plain = cipher.decrypt(&first_ct, &first_nonce, &aad).unwrap();
    let second_plain = cipher.decrypt(&second_ct, &second_nonce, &aad).unwrap();
    assert_eq!(first_plain, b"1//first-refresh".to_vec());
    assert_eq!(second_plain, b"1//second-refresh".to_vec());
    assert_ne!(
        first_plain, second_plain,
        "refresh_token plaintext must rotate on re-authorize"
    );
}

#[tokio::test]
async fn finish_login_surfaces_oauth_error_json_from_4xx() {
    // Regression for Copilot R2: when Google returns HTTP 4xx
    // with an OAuth-shaped error body, the auth crate must
    // surface the `error` / `error_description` instead of
    // collapsing to "token endpoint returned 400". Without the
    // body-first parse, ops would see no actionable detail.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "Bad Request"
        })))
        .mount(&server)
        .await;

    let (svc, _, _, _) = build_service(&server.uri());
    let started = svc.start_login("https://hub.example/cb").await.unwrap();
    let state = token_from_url(&started.redirect_to, "state").unwrap();
    let err = svc
        .finish_login("test-code", &state, "https://hub.example/cb")
        .await
        .unwrap_err();
    assert!(
        matches!(
            &err,
            AuthError::OAuthExchange(msg)
                if msg.contains("invalid_grant") && msg.contains("Bad Request")
        ),
        "expected `invalid_grant (Bad Request)` to surface, got: {err:?}"
    );
}

#[tokio::test]
async fn finish_login_rejects_id_token_with_array_audience() {
    // Regression for Copilot R4 (strict OIDC `aud`): an attacker
    // who can mint a token for some other client where Google
    // lists multiple audiences (including ours) should NOT pass
    // our audience check. The auth crate rejects array-shaped
    // `aud` claims outright — Google never issues those for
    // client OAuth flows.
    #[derive(Serialize)]
    struct ArrayAudClaims<'a> {
        iss: &'a str,
        aud: Vec<&'a str>,
        sub: &'a str,
        email: &'a str,
        email_verified: bool,
        iat: i64,
        exp: i64,
    }
    let now = Utc::now().timestamp();
    let claims = ArrayAudClaims {
        iss: "https://accounts.google.com",
        // `test-client-id` IS in the array, so a loose
        // intersection-style audience check would pass. The
        // strict single-string requirement rejects it.
        aud: vec!["test-client-id", "another-client"],
        sub: "google-sub-array",
        email: "array@example.com",
        email_verified: true,
        iat: now,
        exp: now + 3600,
    };
    let key = EncodingKey::from_rsa_der(&test_signing_der());
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test-key".to_owned());
    let id_token = jsonwebtoken::encode(&header, &claims, &key).expect("sign");

    let server = MockServer::start().await;
    install_google_token_mock(&server, &id_token).await;
    let (svc, _, _, _) = build_service(&server.uri());
    let started = svc.start_login("https://hub.example/cb").await.unwrap();
    let state = token_from_url(&started.redirect_to, "state").unwrap();
    let err = svc
        .finish_login("test-code", &state, "https://hub.example/cb")
        .await
        .unwrap_err();
    assert!(matches!(&err, AuthError::OAuthExchange(_)), "got: {err:?}");
}
