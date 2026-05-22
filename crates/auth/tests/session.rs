//! Integration tests for [`auth::SessionService`] against an
//! in-memory [`SessionRepo`] fake. No real Postgres.
//!
//! The fake mirrors the production `Storage` impl's WHERE
//! predicate exactly: `touch_and_authenticate` returns `Some`
//! ONLY when the row is unrevoked AND unexpired, and updates
//! `last_seen_at` atomically. Anything that fails one of those
//! checks surfaces as `Ok(None)` — same as the SQL version.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use auth::{SessionService, ValidatedSession};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use storage::{AuthenticatedSession, NewSession, SessionRepo, StorageError};
use uuid::Uuid;

// --- in-memory fake -------------------------------------------------

#[derive(Default)]
struct InMemorySessionRepo {
    inner: Mutex<HashMap<Vec<u8>, Row>>,
}

#[derive(Clone)]
struct Row {
    user_id: Uuid,
    created_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
    user_agent: Option<String>,
    ip_addr: Option<IpAddr>,
}

#[async_trait]
impl SessionRepo for InMemorySessionRepo {
    async fn insert_session(&self, new: NewSession) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().unwrap();
        if inner.contains_key(&new.id_hash) {
            return Err(StorageError::UniqueViolation("sessions_pkey".to_owned()));
        }
        let now = Utc::now();
        inner.insert(
            new.id_hash,
            Row {
                user_id: new.user_id,
                created_at: now,
                last_seen_at: now,
                expires_at: new.expires_at,
                revoked_at: None,
                user_agent: new.user_agent,
                ip_addr: new.ip_addr,
            },
        );
        Ok(())
    }

    async fn touch_and_authenticate(
        &self,
        id_hash: &[u8],
        now: DateTime<Utc>,
    ) -> Result<Option<AuthenticatedSession>, StorageError> {
        let mut inner = self.inner.lock().unwrap();
        let Some(row) = inner.get_mut(id_hash) else {
            return Ok(None);
        };
        if row.revoked_at.is_some() || row.expires_at <= now {
            return Ok(None);
        }
        row.last_seen_at = now;
        Ok(Some(AuthenticatedSession {
            user_id: row.user_id,
            created_at: row.created_at,
            expires_at: row.expires_at,
        }))
    }

    async fn revoke_session(
        &self,
        id_hash: &[u8],
        now: DateTime<Utc>,
    ) -> Result<bool, StorageError> {
        let mut inner = self.inner.lock().unwrap();
        let Some(row) = inner.get_mut(id_hash) else {
            return Ok(false);
        };
        if row.revoked_at.is_some() {
            return Ok(false);
        }
        row.revoked_at = Some(now);
        Ok(true)
    }

    async fn revoke_all_sessions_for_user(
        &self,
        user_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<u64, StorageError> {
        let mut inner = self.inner.lock().unwrap();
        let mut count: u64 = 0;
        for row in inner.values_mut() {
            if row.user_id == user_id && row.revoked_at.is_none() {
                row.revoked_at = Some(now);
                count += 1;
            }
        }
        Ok(count)
    }
}

// --- helpers --------------------------------------------------------

fn build_service(ttl: Duration) -> (SessionService, Arc<InMemorySessionRepo>) {
    let repo: Arc<InMemorySessionRepo> = Arc::new(InMemorySessionRepo::default());
    let svc = SessionService::new(repo.clone() as Arc<dyn SessionRepo>).with_ttl(ttl);
    (svc, repo)
}

fn fresh_user_id() -> Uuid {
    Uuid::now_v7()
}

// --- tests ----------------------------------------------------------

#[tokio::test]
async fn issue_then_authenticate_round_trip() {
    let (svc, repo) = build_service(Duration::from_secs(60));
    let user_id = fresh_user_id();
    let issued = svc
        .issue(
            user_id,
            Some("test-agent".to_owned()),
            Some("127.0.0.1".parse().unwrap()),
        )
        .await
        .unwrap();

    // Cookie value is 43 chars (base64url of 32 random bytes).
    assert_eq!(issued.cookie_value.len(), 43);
    assert!(issued.expires_at > Utc::now());

    // Authenticate the cookie back to the same user.
    let session: ValidatedSession = svc
        .authenticate(&issued.cookie_value)
        .await
        .unwrap()
        .expect("session valid");
    assert_eq!(session.user_id, user_id);
    assert_eq!(session.expires_at, issued.expires_at);

    // Audit metadata was persisted alongside the row.
    let inner = repo.inner.lock().unwrap();
    let row = inner.values().next().unwrap();
    assert_eq!(row.user_agent.as_deref(), Some("test-agent"));
    assert!(row.ip_addr.is_some());
}

#[tokio::test]
async fn authenticate_returns_none_for_unknown_cookie() {
    let (svc, _) = build_service(Duration::from_secs(60));
    // A perfectly-shaped but never-issued token.
    let bogus = URL_SAFE_NO_PAD.encode([7u8; 32]);
    assert!(svc.authenticate(&bogus).await.unwrap().is_none());
}

#[tokio::test]
async fn authenticate_returns_none_for_malformed_cookie() {
    let (svc, _) = build_service(Duration::from_secs(60));
    // Empty / non-base64 / wrong-length all collapse to None
    // (not an error) so the gateway treats a stale cookie the
    // same way as no cookie.
    assert!(svc.authenticate("").await.unwrap().is_none());
    assert!(svc.authenticate("!!!notbase64!!!").await.unwrap().is_none());
    assert!(svc.authenticate("AAAA").await.unwrap().is_none());
}

#[tokio::test]
async fn revoke_invalidates_session_immediately() {
    let (svc, _) = build_service(Duration::from_secs(60));
    let issued = svc.issue(fresh_user_id(), None, None).await.unwrap();
    assert!(
        svc.authenticate(&issued.cookie_value)
            .await
            .unwrap()
            .is_some()
    );

    let revoked = svc.revoke(&issued.cookie_value).await.unwrap();
    assert!(revoked, "first revoke flips a row");
    // Re-authenticate after revoke: anonymous.
    assert!(
        svc.authenticate(&issued.cookie_value)
            .await
            .unwrap()
            .is_none()
    );
    // Re-revoke is idempotent.
    assert!(!svc.revoke(&issued.cookie_value).await.unwrap());
}

#[tokio::test]
async fn revoke_returns_false_for_malformed_cookie() {
    let (svc, _) = build_service(Duration::from_secs(60));
    // Bad cookie shouldn't error — just no-op.
    assert!(!svc.revoke("not-a-token").await.unwrap());
}

#[tokio::test]
async fn revoke_all_for_user_kills_every_active_session() {
    let (svc, _) = build_service(Duration::from_secs(60));
    let user_id = fresh_user_id();
    let other = fresh_user_id();
    let a = svc.issue(user_id, None, None).await.unwrap();
    let b = svc.issue(user_id, None, None).await.unwrap();
    let c = svc.issue(other, None, None).await.unwrap();

    let killed = svc.revoke_all_for_user(user_id).await.unwrap();
    assert_eq!(killed, 2, "both sessions for target user revoked");

    assert!(svc.authenticate(&a.cookie_value).await.unwrap().is_none());
    assert!(svc.authenticate(&b.cookie_value).await.unwrap().is_none());
    // The unrelated user's session survived.
    assert!(svc.authenticate(&c.cookie_value).await.unwrap().is_some());

    // Calling again returns 0 — idempotent.
    assert_eq!(svc.revoke_all_for_user(user_id).await.unwrap(), 0);
}

#[tokio::test]
async fn authenticate_returns_none_after_expiry() {
    // 1 ms TTL forces the row to be expired by the time we
    // authenticate it back.
    let (svc, repo) = build_service(Duration::from_millis(1));
    let issued = svc.issue(fresh_user_id(), None, None).await.unwrap();
    // Walk the row's `expires_at` backward to make the test
    // deterministic — the production `touch_and_authenticate`
    // SQL also keys on `expires_at > now`, so this exercises
    // the same predicate path without sleeping.
    {
        let mut inner = repo.inner.lock().unwrap();
        let row = inner.values_mut().next().unwrap();
        row.expires_at = Utc::now() - chrono::Duration::seconds(1);
    }
    assert!(
        svc.authenticate(&issued.cookie_value)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn two_issued_sessions_get_distinct_cookies() {
    // Random nonce → no two `issue` calls return the same
    // cookie even for the same user.
    let (svc, _) = build_service(Duration::from_secs(60));
    let user_id = fresh_user_id();
    let a = svc.issue(user_id, None, None).await.unwrap();
    let b = svc.issue(user_id, None, None).await.unwrap();
    assert_ne!(a.cookie_value, b.cookie_value);
    // Both authenticate to the same user.
    assert_eq!(
        svc.authenticate(&a.cookie_value)
            .await
            .unwrap()
            .unwrap()
            .user_id,
        svc.authenticate(&b.cookie_value)
            .await
            .unwrap()
            .unwrap()
            .user_id,
    );
}
