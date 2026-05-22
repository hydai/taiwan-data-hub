//! End-to-end tests for [`AuthService`] against in-memory repo
//! fakes + the [`MemoryMailer`]. No Postgres / SMTP required, so
//! the suite runs in plain `cargo test` and exercises the real
//! token + hashing code paths.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use auth::{AuthError, AuthService, MailKind, MemoryMailer};
use chrono::{DateTime, Utc};
use storage::{AuthTokenKind, AuthTokenRepo, StorageError, User, UserRepo};
use url::Url;
use uuid::Uuid;

// --- in-memory repos -------------------------------------------------

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
            return Err(StorageError::UniqueViolation(format!(
                "users_email_key on {email}"
            )));
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
        let inner = self.inner.lock().unwrap();
        Ok(inner.get(&id).cloned())
    }

    async fn mark_email_verified(&self, id: Uuid) -> Result<bool, StorageError> {
        let mut inner = self.inner.lock().unwrap();
        let Some(user) = inner.get_mut(&id) else {
            return Ok(false);
        };
        if user.email_verified_at.is_some() {
            return Ok(false);
        }
        user.email_verified_at = Some(Utc::now());
        Ok(true)
    }

    async fn update_password_hash(
        &self,
        id: Uuid,
        password_hash: &str,
    ) -> Result<bool, StorageError> {
        let mut inner = self.inner.lock().unwrap();
        let Some(user) = inner.get_mut(&id) else {
            return Ok(false);
        };
        password_hash.clone_into(&mut user.password_hash);
        Ok(true)
    }

    async fn delete_user(&self, id: Uuid) -> Result<bool, StorageError> {
        Ok(self.inner.lock().unwrap().remove(&id).is_some())
    }
}

#[derive(Default)]
struct InMemoryAuthTokenRepo {
    /// Keyed by `token_hash` so the consume path is O(1).
    inner: Mutex<HashMap<Vec<u8>, TokenRow>>,
}

/// `UserRepo` over an `Arc<InMemoryUserRepo>` so a test can hold a
/// second handle and mutate state out-of-band (e.g. delete a row
/// after a token is consumed). Lives at module scope because the
/// workspace's clippy gate rejects items declared after statements
/// inside a test function.
struct ArcUserRepo(std::sync::Arc<InMemoryUserRepo>);

#[async_trait]
impl UserRepo for ArcUserRepo {
    async fn insert_user(&self, email: &str, hash: &str) -> Result<User, StorageError> {
        self.0.insert_user(email, hash).await
    }
    async fn find_user_by_email(&self, email: &str) -> Result<Option<User>, StorageError> {
        self.0.find_user_by_email(email).await
    }
    async fn find_user_by_id(&self, id: Uuid) -> Result<Option<User>, StorageError> {
        self.0.find_user_by_id(id).await
    }
    async fn mark_email_verified(&self, id: Uuid) -> Result<bool, StorageError> {
        self.0.mark_email_verified(id).await
    }
    async fn update_password_hash(&self, id: Uuid, hash: &str) -> Result<bool, StorageError> {
        self.0.update_password_hash(id, hash).await
    }
    async fn delete_user(&self, id: Uuid) -> Result<bool, StorageError> {
        self.0.delete_user(id).await
    }
}

#[derive(Clone)]
struct TokenRow {
    user_id: Uuid,
    kind: AuthTokenKind,
    expires_at: DateTime<Utc>,
    consumed_at: Option<DateTime<Utc>>,
}

#[async_trait]
impl AuthTokenRepo for InMemoryAuthTokenRepo {
    async fn insert_auth_token(
        &self,
        user_id: Uuid,
        kind: AuthTokenKind,
        token_hash: &[u8],
        expires_at: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().unwrap();
        if inner.contains_key(token_hash) {
            // Match Postgres `auth_tokens.token_hash UNIQUE` so a
            // bug that re-uses a hash surfaces in tests instead of
            // silently overwriting state.
            return Err(StorageError::UniqueViolation(
                "auth_tokens_token_hash_key".to_owned(),
            ));
        }
        inner.insert(
            token_hash.to_vec(),
            TokenRow {
                user_id,
                kind,
                expires_at,
                consumed_at: None,
            },
        );
        Ok(())
    }

    async fn consume_auth_token(
        &self,
        kind: AuthTokenKind,
        token_hash: &[u8],
        now: DateTime<Utc>,
    ) -> Result<Option<Uuid>, StorageError> {
        let mut inner = self.inner.lock().unwrap();
        let Some(row) = inner.get_mut(token_hash) else {
            return Ok(None);
        };
        if row.kind != kind || row.consumed_at.is_some() || row.expires_at <= now {
            return Ok(None);
        }
        row.consumed_at = Some(now);
        Ok(Some(row.user_id))
    }

    async fn invalidate_user_tokens(
        &self,
        user_id: Uuid,
        kind: AuthTokenKind,
        now: DateTime<Utc>,
    ) -> Result<u64, StorageError> {
        let mut inner = self.inner.lock().unwrap();
        let mut count = 0u64;
        for row in inner.values_mut() {
            if row.user_id == user_id && row.kind == kind && row.consumed_at.is_none() {
                row.consumed_at = Some(now);
                count += 1;
            }
        }
        Ok(count)
    }
}

// --- helpers ---------------------------------------------------------

type Svc = AuthService<InMemoryUserRepo, InMemoryAuthTokenRepo, MemoryMailer>;

fn build_service() -> (Svc, MemoryMailer) {
    let users = InMemoryUserRepo::default();
    let tokens = InMemoryAuthTokenRepo::default();
    let mailer = MemoryMailer::new();
    let base = Url::parse("https://hub.example").unwrap();
    let svc = AuthService::new(users, tokens, mailer.clone(), base);
    (svc, mailer)
}

/// Wait until `mailer` has recorded at least `expected` sends, or
/// panic after 2 seconds. `AuthService` now spawns the SMTP send so
/// the caller-visible response doesn't leak SMTP latency; the test
/// has to give the runtime a chance to drive the spawned task to
/// completion before asserting on `mailer.sent()`.
async fn wait_for_mailer(mailer: &MemoryMailer, expected: usize) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while mailer.sent().len() < expected {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for mailer to record {expected} sends (got {})",
            mailer.sent().len(),
        );
        tokio::task::yield_now().await;
    }
}

fn token_from_link(link: &Url) -> String {
    link.query_pairs()
        .find(|(k, _)| k == "token")
        .map(|(_, v)| v.into_owned())
        .expect("magic link carries ?token=…")
}

// --- tests -----------------------------------------------------------

#[tokio::test]
async fn register_creates_user_and_sends_verification_link() {
    let (svc, mailer) = build_service();
    svc.register("alice@example.com", "very-long-passphrase")
        .await
        .unwrap();
    wait_for_mailer(&mailer, 1).await;

    let sent = mailer.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, "alice@example.com");
    assert_eq!(sent[0].kind, MailKind::Verification);
    assert!(
        sent[0]
            .link
            .as_str()
            .starts_with("https://hub.example/auth/verify?token=")
    );
}

/// `AuthTokenRepo` whose `insert_auth_token` always returns an
/// error. Used to drive the `register` compensating-delete path.
#[derive(Default)]
struct FailingTokenRepo;

#[async_trait]
impl AuthTokenRepo for FailingTokenRepo {
    async fn insert_auth_token(
        &self,
        _user_id: Uuid,
        _kind: AuthTokenKind,
        _token_hash: &[u8],
        _expires_at: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        Err(StorageError::InvalidArgument(
            "fake token insert failure".to_owned(),
        ))
    }
    async fn consume_auth_token(
        &self,
        _kind: AuthTokenKind,
        _token_hash: &[u8],
        _now: DateTime<Utc>,
    ) -> Result<Option<Uuid>, StorageError> {
        Ok(None)
    }
    async fn invalidate_user_tokens(
        &self,
        _user_id: Uuid,
        _kind: AuthTokenKind,
        _now: DateTime<Utc>,
    ) -> Result<u64, StorageError> {
        Ok(0)
    }
}

#[tokio::test]
async fn register_compensates_with_delete_when_token_insert_fails() {
    // Regression for Copilot R4: if the token insert fails after the
    // user row is persisted, `register` must clean up the row so a
    // retry doesn't hit `EmailTaken` against an orphaned account.
    // The InMemoryUserRepo is held via Arc so the test can inspect
    // the post-compensation state independently of the service.
    let users = std::sync::Arc::new(InMemoryUserRepo::default());
    let tokens = FailingTokenRepo;
    let mailer = MemoryMailer::new();
    let base = Url::parse("https://hub.example").unwrap();
    let svc = AuthService::new(ArcUserRepo(users.clone()), tokens, mailer.clone(), base);

    let err = svc
        .register("orphan@example.com", "passphrase")
        .await
        .unwrap_err();
    // Failure propagated from the storage layer (we faked it).
    assert!(matches!(err, AuthError::Storage(_)));
    // No mail sent because we never reached the spawn.
    assert!(mailer.sent().is_empty());
    // Most important: the orphaned row was deleted by the
    // compensation step, so a retry wouldn't see `EmailTaken`.
    assert!(
        users
            .find_user_by_email("orphan@example.com")
            .await
            .unwrap()
            .is_none(),
        "compensating delete should have removed the orphaned user"
    );
}

#[tokio::test]
async fn register_with_taken_email_surfaces_email_taken() {
    let (svc, _) = build_service();
    svc.register("alice@example.com", "first-passphrase")
        .await
        .unwrap();
    let err = svc
        .register("ALICE@example.com", "second-passphrase")
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::EmailTaken));
}

#[tokio::test]
async fn verify_email_consumes_token_and_marks_user_verified() {
    let (svc, mailer) = build_service();
    svc.register("bob@example.com", "passphrase").await.unwrap();
    wait_for_mailer(&mailer, 1).await;
    let token = token_from_link(&mailer.sent()[0].link);

    svc.verify_email(&token).await.unwrap();

    // Replay must fail — single-use.
    let err = svc.verify_email(&token).await.unwrap_err();
    assert!(matches!(err, AuthError::InvalidToken));
}

#[tokio::test]
async fn verify_email_rejects_unknown_token() {
    let (svc, _) = build_service();
    let err = svc.verify_email("not-a-real-token").await.unwrap_err();
    assert!(matches!(err, AuthError::InvalidToken));
}

#[tokio::test]
async fn login_returns_redacted_user_on_correct_password() {
    let (svc, _) = build_service();
    svc.register("carol@example.com", "passphrase")
        .await
        .unwrap();
    let authed = svc.login("carol@example.com", "passphrase").await.unwrap();
    assert_eq!(authed.email, "carol@example.com");
    // `AuthenticatedUser` deliberately has no `password_hash` field
    // — that's the entire point of the redacted DTO. If a future
    // refactor adds it back, this test won't compile.
    assert!(authed.email_verified_at.is_none());
}

#[tokio::test]
async fn login_rejects_wrong_password_with_invalid_credentials() {
    let (svc, _) = build_service();
    svc.register("dan@example.com", "passphrase").await.unwrap();
    let err = svc.login("dan@example.com", "WRONG").await.unwrap_err();
    assert!(matches!(err, AuthError::InvalidCredentials));
}

#[tokio::test]
async fn login_returns_invalid_credentials_when_stored_hash_is_corrupt() {
    // Regression for Copilot R1 round 1 — a malformed `password_hash`
    // (e.g. hand-edited row, lost migration) must NOT surface as
    // AuthError::PasswordHash to the login caller, since that would
    // make the row's presence distinguishable from a normal mismatch.
    let users = InMemoryUserRepo::default();
    let tokens = InMemoryAuthTokenRepo::default();
    let mailer = MemoryMailer::new();
    let base = Url::parse("https://hub.example").unwrap();

    // Seed a row whose password_hash is NOT a valid PHC string.
    users
        .insert_user("corrupt@example.com", "not-a-phc-string")
        .await
        .unwrap();
    let svc = AuthService::new(users, tokens, mailer, base);

    let err = svc
        .login("corrupt@example.com", "anything")
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::InvalidCredentials));
}

#[tokio::test]
async fn login_with_unknown_email_returns_invalid_credentials_not_email_taken() {
    let (svc, _) = build_service();
    let err = svc
        .login("ghost@example.com", "anything")
        .await
        .unwrap_err();
    // The variant must be InvalidCredentials, NOT EmailTaken — the
    // public response cannot distinguish "user unknown" from "wrong
    // password" or enumeration is trivial.
    assert!(matches!(err, AuthError::InvalidCredentials));
}

#[tokio::test]
async fn request_password_reset_sends_link_for_known_user() {
    let (svc, mailer) = build_service();
    svc.register("eve@example.com", "passphrase").await.unwrap();
    wait_for_mailer(&mailer, 1).await;
    let pre = mailer.sent().len();
    svc.request_password_reset("eve@example.com").await.unwrap();
    wait_for_mailer(&mailer, pre + 1).await;
    let sent = mailer.sent();
    assert_eq!(sent.len() - pre, 1);
    let last = sent.last().unwrap();
    assert_eq!(last.kind, MailKind::PasswordReset);
    assert!(
        last.link
            .as_str()
            .starts_with("https://hub.example/auth/reset?token=")
    );
}

#[tokio::test]
async fn request_password_reset_returns_ok_for_unknown_user() {
    let (svc, mailer) = build_service();
    // Unknown email — must return Ok and NOT send mail. Yield twice
    // so any (incorrectly) spawned mail send would have a chance to
    // run and trip the assertion.
    svc.request_password_reset("nobody@example.com")
        .await
        .unwrap();
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    assert!(mailer.sent().is_empty());
}

#[tokio::test]
async fn second_password_reset_request_invalidates_the_first() {
    // Regression for Copilot R9: an intercepted older reset link
    // must NOT remain valid once the user has requested a fresh
    // one. Two `request_password_reset` calls in a row should
    // leave only the second link working.
    let (svc, mailer) = build_service();
    svc.register("ivy@example.com", "passphrase").await.unwrap();
    wait_for_mailer(&mailer, 1).await;

    svc.request_password_reset("ivy@example.com").await.unwrap();
    wait_for_mailer(&mailer, 2).await;
    let first_link = mailer
        .sent()
        .into_iter()
        .find(|m| m.kind == MailKind::PasswordReset)
        .unwrap()
        .link;
    let first_token = token_from_link(&first_link);

    svc.request_password_reset("ivy@example.com").await.unwrap();
    wait_for_mailer(&mailer, 3).await;
    let second_link = mailer
        .sent()
        .into_iter()
        .rfind(|m| m.kind == MailKind::PasswordReset)
        .unwrap()
        .link;
    let second_token = token_from_link(&second_link);
    assert_ne!(first_token, second_token);

    // The first link is now invalid.
    let err = svc
        .reset_password(&first_token, "n3w-passphrase")
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::InvalidToken));

    // The second link still works.
    svc.reset_password(&second_token, "n3w-passphrase")
        .await
        .unwrap();
    svc.login("ivy@example.com", "n3w-passphrase")
        .await
        .unwrap();
}

#[tokio::test]
async fn reset_password_surfaces_internal_when_user_disappears() {
    // Regression for Copilot R2: a `reset_password` call that
    // consumed a valid token but then found the user row gone
    // used to return `Ok(())` silently. It now bubbles as
    // `AuthError::Internal` so operators see the race.
    let users = std::sync::Arc::new(InMemoryUserRepo::default());
    let tokens = InMemoryAuthTokenRepo::default();
    let mailer = MemoryMailer::new();
    let base = Url::parse("https://hub.example").unwrap();
    // Seed user manually, mint a reset token, then drop the row.
    let user = users
        .insert_user("ghost@example.com", "opaque-placeholder-not-a-real-phc")
        .await
        .unwrap();
    let token = auth::generate_token();
    tokens
        .insert_auth_token(
            user.id,
            AuthTokenKind::PasswordReset,
            &token.digest,
            Utc::now() + chrono::Duration::hours(1),
        )
        .await
        .unwrap();
    users.inner.lock().unwrap().remove(&user.id);

    // `ArcUserRepo` (at module scope) delegates to the same
    // `Arc<InMemoryUserRepo>` so the service sees the post-delete
    // state when it tries `update_password_hash`.
    let svc = AuthService::new(ArcUserRepo(users), tokens, mailer, base);

    let err = svc
        .reset_password(&token.cleartext, "new-passphrase")
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::Internal(_)));
}

#[tokio::test]
async fn reset_password_consumes_token_and_updates_hash() {
    let (svc, mailer) = build_service();
    svc.register("frank@example.com", "old-passphrase")
        .await
        .unwrap();
    wait_for_mailer(&mailer, 1).await;
    svc.request_password_reset("frank@example.com")
        .await
        .unwrap();
    wait_for_mailer(&mailer, 2).await;
    let reset_link = mailer
        .sent()
        .into_iter()
        .find(|m| m.kind == MailKind::PasswordReset)
        .unwrap()
        .link;
    let token = token_from_link(&reset_link);

    svc.reset_password(&token, "new-passphrase").await.unwrap();

    // Old password no longer logs in; new password does.
    let err = svc
        .login("frank@example.com", "old-passphrase")
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::InvalidCredentials));
    svc.login("frank@example.com", "new-passphrase")
        .await
        .unwrap();

    // Replay of the reset link is rejected — single-use.
    let err = svc
        .reset_password(&token, "yet-another-passphrase")
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::InvalidToken));
}

#[tokio::test]
async fn resend_verification_is_silent_for_unknown_or_verified_users() {
    let (svc, mailer) = build_service();
    svc.resend_verification("ghost@example.com").await.unwrap();
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    assert!(mailer.sent().is_empty(), "no mail for unknown user");

    svc.register("grace@example.com", "passphrase")
        .await
        .unwrap();
    wait_for_mailer(&mailer, 1).await;
    let token = token_from_link(&mailer.sent()[0].link);
    svc.verify_email(&token).await.unwrap();
    let before = mailer.sent().len();
    svc.resend_verification("grace@example.com").await.unwrap();
    // No mail should arrive — a yield twice gives a stray spawn a
    // chance to trip the assertion below.
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    assert_eq!(
        mailer.sent().len(),
        before,
        "no resend after the address is verified"
    );
}
