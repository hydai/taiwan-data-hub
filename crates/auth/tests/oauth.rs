//! End-to-end tests for [`auth::OAuthService`] against
//! in-memory repo fakes + a wiremock-backed fake GitHub.
//! No real Postgres / no real GitHub.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use auth::{AuthError, GitHubProvider, OAuthService, TokenCipher};
use chrono::{DateTime, Utc};
use reqwest::Client;
use storage::{
    NewOAuthAccount, OAuthAccountRepo, OAuthPendingState, OAuthStateRepo, StorageError, User,
    UserRepo,
};
use url::Url;
use uuid::Uuid;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// --- in-memory fakes -------------------------------------------------

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
        // Peek before removing so a wrong-provider / wrong-
        // redirect_uri callback can't consume the legitimate
        // user's row. Matches the production
        // DELETE … WHERE provider = $2 AND redirect_uri = $3.
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
            // Mirror the production SQL: keep the original
            // `user_id`, rotate every other field.
            existing.access_token_ciphertext = new.access_token_ciphertext;
            existing.access_token_nonce = new.access_token_nonce;
            existing.refresh_token_ciphertext = new.refresh_token_ciphertext;
            existing.refresh_token_nonce = new.refresh_token_nonce;
            existing.expires_at = new.expires_at;
        } else {
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

// --- helpers ---------------------------------------------------------

fn test_kek() -> [u8; 32] {
    [42u8; 32]
}

fn build_service(
    mock_url: &str,
) -> (
    OAuthService<GitHubProvider>,
    Arc<InMemoryUserRepo>,
    Arc<InMemoryOAuthAccountRepo>,
    Arc<InMemoryOAuthStateRepo>,
) {
    let users: Arc<InMemoryUserRepo> = Arc::new(InMemoryUserRepo::default());
    let states: Arc<InMemoryOAuthStateRepo> = Arc::new(InMemoryOAuthStateRepo::default());
    let accounts: Arc<InMemoryOAuthAccountRepo> = Arc::new(InMemoryOAuthAccountRepo::default());
    let provider = GitHubProvider::with_endpoints(
        "test-client-id".to_owned(),
        "test-client-secret".to_owned(),
        Client::new(),
        format!("{mock_url}/login/oauth/authorize"),
        format!("{mock_url}/login/oauth/access_token"),
        mock_url.to_owned(),
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

async fn install_github_mocks(server: &MockServer, returned_email: &str, github_user_id: u64) {
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "gho_test-access-token-value",
            "token_type": "bearer",
            "scope": "read:user user:email"
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": github_user_id })),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/user/emails"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "email": "secondary@example.com",
                "primary": false,
                "verified": true
            },
            {
                "email": returned_email,
                "primary": true,
                "verified": true
            }
        ])))
        .mount(server)
        .await;
}

fn token_from_url(url: &Url, key: &str) -> Option<String> {
    url.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

// --- tests -----------------------------------------------------------

#[tokio::test]
async fn start_login_returns_authorize_url_with_pkce_and_state() {
    let server = MockServer::start().await;
    let (svc, _, _, states) = build_service(&server.uri());
    let started = svc
        .start_login("https://hub.example/auth/callback")
        .await
        .unwrap();
    let url = started.redirect_to;
    assert!(url.as_str().contains("/login/oauth/authorize"));
    let state = token_from_url(&url, "state").expect("state on authorize URL");
    let challenge = token_from_url(&url, "code_challenge").expect("challenge on authorize URL");
    assert_eq!(
        token_from_url(&url, "code_challenge_method").as_deref(),
        Some("S256")
    );
    assert_eq!(
        token_from_url(&url, "client_id").as_deref(),
        Some("test-client-id")
    );
    // PKCE challenge is 43 chars (sha256 → 32 bytes → b64url-no-pad).
    assert_eq!(challenge.len(), 43);
    // Exactly one pending state was persisted.
    assert_eq!(states.inner.lock().unwrap().len(), 1);
    // And the cleartext state matches the persisted hash.
    let hash = auth::hash_state(&state);
    assert!(states.inner.lock().unwrap().contains_key(&hash));
}

#[tokio::test]
async fn finish_login_creates_new_user_when_no_match() {
    let server = MockServer::start().await;
    install_github_mocks(&server, "alice@example.com", 12345).await;
    let (svc, users, accounts, _) = build_service(&server.uri());

    let started = svc.start_login("https://hub.example/cb").await.unwrap();
    let state = token_from_url(&started.redirect_to, "state").unwrap();

    let authed = svc
        .finish_login("test-code", &state, "https://hub.example/cb")
        .await
        .unwrap();
    assert_eq!(authed.email, "alice@example.com");
    assert!(
        authed.email_verified_at.is_some(),
        "OAuth-created user should be verified"
    );

    // User was created.
    assert!(
        users
            .find_user_by_email("alice@example.com")
            .await
            .unwrap()
            .is_some()
    );
    // Account row exists with encrypted token.
    let account = accounts
        .inner
        .lock()
        .unwrap()
        .get(&("github".to_owned(), "12345".to_owned()))
        .cloned()
        .expect("account row");
    assert!(!account.access_token_ciphertext.is_empty());
    assert_eq!(account.access_token_nonce.len(), 12);
    // The cleartext token is NOT recorded anywhere we can see in
    // the account row — `gho_test-access-token-value` should
    // appear only inside the AES-GCM-encrypted bytes. A naive
    // memmem check covers the obvious leak.
    assert!(
        !account
            .access_token_ciphertext
            .windows(b"gho_test".len())
            .any(|w| w == b"gho_test"),
        "raw access token must not appear in ciphertext bytes"
    );
}

#[tokio::test]
async fn finish_login_links_to_existing_user_by_email() {
    let server = MockServer::start().await;
    install_github_mocks(&server, "bob@example.com", 67890).await;
    let (svc, users, accounts, _) = build_service(&server.uri());

    // Seed a user that registered through the email/password path.
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
    assert_eq!(
        authed.id, preexisting.id,
        "should link to existing user, not create one"
    );
    // The provider attested the address — the prior unverified
    // row is now verified.
    assert!(authed.email_verified_at.is_some());
    // Account row exists.
    assert!(
        accounts
            .inner
            .lock()
            .unwrap()
            .contains_key(&("github".to_owned(), "67890".to_owned()))
    );
}

#[tokio::test]
async fn finish_login_with_unknown_state_returns_invalid_state() {
    let server = MockServer::start().await;
    install_github_mocks(&server, "carol@example.com", 1).await;
    let (svc, _, _, _) = build_service(&server.uri());

    // No start_login first — the state token has never been issued.
    let err = svc
        .finish_login("test-code", "made-up-state-token", "https://hub.example/cb")
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::InvalidState));
}

#[tokio::test]
async fn finish_login_replay_returns_invalid_state() {
    let server = MockServer::start().await;
    install_github_mocks(&server, "dan@example.com", 2).await;
    let (svc, _, _, _) = build_service(&server.uri());

    let started = svc.start_login("https://hub.example/cb").await.unwrap();
    let state = token_from_url(&started.redirect_to, "state").unwrap();
    svc.finish_login("test-code", &state, "https://hub.example/cb")
        .await
        .unwrap();
    // Replay of the same state.
    let err = svc
        .finish_login("test-code", &state, "https://hub.example/cb")
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::InvalidState));
}

#[tokio::test]
async fn provider_identity_stays_bound_to_original_user_when_email_changes() {
    // Regression for Copilot R2: an attacker who controls the
    // GitHub side could change their email to match an existing
    // local user. The OAuth identity must NOT migrate via that
    // path — look-up by (provider, provider_user_id) wins over
    // email-based linking.
    let server = MockServer::start().await;
    let (svc, users, accounts, _) = build_service(&server.uri());

    // First login: GitHub returns email "alice@example.com" and
    // user id 9999. A fresh `users` row is created.
    install_github_mocks(&server, "alice@example.com", 9999).await;
    let started = svc.start_login("https://hub.example/cb").await.unwrap();
    let state = token_from_url(&started.redirect_to, "state").unwrap();
    let alice = svc
        .finish_login("code1", &state, "https://hub.example/cb")
        .await
        .unwrap();

    // Free Alice's email at the local-user layer (simulating
    // a rename), then seed a second local user with the
    // original `alice@example.com`. The GitHub mock STILL
    // returns alice@example.com + provider_user_id=9999 on
    // the second login — exactly the attack shape: the
    // provider claims the collision-target email, but
    // identity-stability must still land on the original
    // Alice row.
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
        .finish_login("code2", &state, "https://hub.example/cb")
        .await
        .unwrap();

    assert_eq!(
        alice_again.id, alice.id,
        "GitHub identity must stay bound to original user"
    );
    assert_ne!(alice_again.id, snitch_id);

    // One account row, still pointing at Alice.
    let row = accounts
        .inner
        .lock()
        .unwrap()
        .get(&("github".to_owned(), "9999".to_owned()))
        .cloned()
        .expect("account row");
    assert_eq!(row.user_id, alice.id);
}

#[tokio::test]
async fn finish_login_rejects_mismatched_redirect_uri() {
    let server = MockServer::start().await;
    install_github_mocks(&server, "eve@example.com", 3).await;
    let (svc, _, _, _) = build_service(&server.uri());

    let started = svc.start_login("https://hub.example/cb").await.unwrap();
    let state = token_from_url(&started.redirect_to, "state").unwrap();
    // Callback comes back with a DIFFERENT redirect_uri.
    let err = svc
        .finish_login("test-code", &state, "https://attacker.example/cb")
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::InvalidState));
}
