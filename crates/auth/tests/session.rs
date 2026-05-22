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
use sha2::{Digest, Sha256};
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
        new_expires_at: DateTime<Utc>,
    ) -> Result<Option<AuthenticatedSession>, StorageError> {
        let mut inner = self.inner.lock().unwrap();
        let Some(row) = inner.get_mut(id_hash) else {
            return Ok(None);
        };
        if row.revoked_at.is_some() || row.expires_at <= now {
            return Ok(None);
        }
        // Sliding-window refresh: extend `expires_at` to the
        // service-provided new value (matches the production SQL
        // `SET expires_at = $3`).
        row.last_seen_at = now;
        row.expires_at = new_expires_at;
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
            // Mirror the production WHERE: `revoked_at IS NULL
            // AND expires_at > $now`. Already-expired rows stay
            // un-touched.
            if row.user_id == user_id && row.revoked_at.is_none() && row.expires_at > now {
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
    // Sliding window: authenticate touches the row and extends
    // `expires_at`, so the validated value is >= the issued
    // value (the second `Utc::now()` always >= the first).
    assert!(session.expires_at >= issued.expires_at);

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

#[tokio::test]
async fn authenticate_slides_expires_at_on_each_access() {
    // Regression for Copilot R1 (sliding window): every
    // authenticated request must push `expires_at` forward to
    // `now + ttl`. Walk the stored row's `expires_at` BACKWARD
    // (without touching `revoked_at` or pushing it past `now`)
    // and re-authenticate — the expiry should be back to the
    // freshly-extended value, not the pre-walk one.
    let (svc, repo) = build_service(Duration::from_secs(60));
    let issued = svc.issue(fresh_user_id(), None, None).await.unwrap();
    let original_expiry = issued.expires_at;

    // Walk the row's `expires_at` backward by ~30s, still
    // unexpired. After the next authenticate the expiry should
    // jump forward to roughly `now + 60s` (well past the walked
    // value).
    {
        let mut inner = repo.inner.lock().unwrap();
        let row = inner.values_mut().next().unwrap();
        row.expires_at = Utc::now() + chrono::Duration::seconds(30);
    }
    let validated = svc
        .authenticate(&issued.cookie_value)
        .await
        .unwrap()
        .expect("session still valid");
    let walked_window_max = Utc::now() + chrono::Duration::seconds(30);
    assert!(
        validated.expires_at > walked_window_max,
        "expires_at must slide forward to ~now+ttl, got {} (walked max {walked_window_max})",
        validated.expires_at
    );
    // And the persisted row's expiry matches what the service
    // returned.
    let inner = repo.inner.lock().unwrap();
    let row = inner.values().next().unwrap();
    assert_eq!(row.expires_at, validated.expires_at);

    // The original_expiry is still close in time to validated.
    // expires_at because the TTL is the same — but they're not
    // EQUAL, since `Utc::now()` advanced between the two issue
    // points. We just need to prove the slide happened, not
    // pin the exact value.
    drop(inner);
    assert_ne!(
        original_expiry, validated.expires_at,
        "even with the same TTL, the second `now` differs from the first"
    );
}

#[tokio::test]
async fn revoke_all_for_user_skips_already_expired_sessions() {
    // Regression for Copilot R1 (`revoke_all_sessions_for_user`
    // wording): the trait says "every active session"; the SQL
    // (and matching fake) must NOT bump `revoked_at` on rows
    // that are already expired — both because that's what
    // "active" means and because the count returned would
    // otherwise lie about new state changes.
    let (svc, repo) = build_service(Duration::from_secs(60));
    let user_id = fresh_user_id();
    let live = svc.issue(user_id, None, None).await.unwrap();
    let dead = svc.issue(user_id, None, None).await.unwrap();
    // Backdate the `dead` row's expires_at so it's already
    // expired. Identifying the row by hash (re-applying the
    // sha256 the service used at insert time) avoids depending
    // on HashMap iteration order.
    let dead_hash = {
        let bytes = URL_SAFE_NO_PAD.decode(&dead.cookie_value).unwrap();
        Sha256::digest(&bytes).to_vec()
    };
    {
        let mut inner = repo.inner.lock().unwrap();
        let row = inner.get_mut(&dead_hash).unwrap();
        row.expires_at = Utc::now() - chrono::Duration::seconds(1);
    }

    // Only the live session counts toward the revoke.
    let killed = svc.revoke_all_for_user(user_id).await.unwrap();
    assert_eq!(killed, 1, "only the still-active session is revoked");

    // Live → now anonymous.
    assert!(
        svc.authenticate(&live.cookie_value)
            .await
            .unwrap()
            .is_none()
    );
    // The dead row is unchanged (still revoked_at = None,
    // expired) — re-authenticate it: also None (expired).
    assert!(
        svc.authenticate(&dead.cookie_value)
            .await
            .unwrap()
            .is_none()
    );
}
