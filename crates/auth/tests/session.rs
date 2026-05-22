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
    absolute_expires_at: DateTime<Utc>,
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
                absolute_expires_at: new.absolute_expires_at,
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
        if row.revoked_at.is_some() || row.expires_at <= now || row.absolute_expires_at <= now {
            return Ok(None);
        }
        // Monotonic sliding-window refresh capped at the
        // absolute expiry — mirrors the production SQL
        // `SET expires_at = LEAST(GREATEST($3, expires_at),
        // absolute_expires_at)`. `GREATEST` defends against a
        // racing earlier request from shrinking `expires_at`;
        // `LEAST` enforces the hard cap.
        row.last_seen_at = now;
        row.expires_at = new_expires_at
            .max(row.expires_at)
            .min(row.absolute_expires_at);
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
    // Idle == absolute by default so the existing tests treat the
    // session as "valid up to `ttl` from creation" without
    // needing the more nuanced sliding-with-cap semantics. The
    // dedicated `authenticate_slides_*` test below overrides the
    // pair explicitly.
    let svc = SessionService::new(repo.clone() as Arc<dyn SessionRepo>, vec![7u8; 32])
        .expect("hmac key valid")
        .with_idle_ttl(ttl)
        .with_absolute_max(ttl);
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

    // Cookie value is `<base64url(token)>.<base64url(hmac)>` —
    // 43 chars (32 random token bytes) + `.` + 43 chars
    // (HMAC-SHA-256 tag, also 32 bytes).
    let (token, tag) = issued.cookie_value.split_once('.').unwrap();
    assert_eq!(token.len(), 43, "token base64url is 43 chars");
    assert_eq!(tag.len(), 43, "HMAC-SHA256 tag base64url is 43 chars");
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
    // Two services that share an HMAC key but NOT a session
    // repo. A cookie issued by `victim` won't be in `attacker`'s
    // repo, so even though the HMAC check passes the DB lookup
    // fails — which proves the unforgeability isn't relying on
    // HMAC alone.
    let victim_repo: Arc<InMemorySessionRepo> = Arc::new(InMemorySessionRepo::default());
    let attacker_repo: Arc<InMemorySessionRepo> = Arc::new(InMemorySessionRepo::default());
    let key = vec![7u8; 32];
    let victim = SessionService::new(victim_repo.clone() as Arc<dyn SessionRepo>, key.clone())
        .unwrap()
        .with_idle_ttl(Duration::from_secs(60))
        .with_absolute_max(Duration::from_secs(60));
    let attacker = SessionService::new(attacker_repo as Arc<dyn SessionRepo>, key)
        .unwrap()
        .with_idle_ttl(Duration::from_secs(60))
        .with_absolute_max(Duration::from_secs(60));
    let issued = victim.issue(fresh_user_id(), None, None).await.unwrap();
    assert!(
        attacker
            .authenticate(&issued.cookie_value)
            .await
            .unwrap()
            .is_none()
    );
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
    // Walk the row's `expires_at` backward to make the test
    // deterministic — the production `touch_and_authenticate`
    // SQL keys on `expires_at > now`, so this exercises the
    // same predicate path without sleeping. The build_service
    // TTL doesn't matter; we override the row directly.
    let (svc, repo) = build_service(Duration::from_secs(60));
    let issued = svc.issue(fresh_user_id(), None, None).await.unwrap();
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
    // Regression for Copilot R1 (sliding window) + R2 (cap):
    // when `idle_ttl < absolute_max`, every authenticated
    // request must push `expires_at` forward by ~idle_ttl,
    // capped at `absolute_expires_at`. Walk the stored row's
    // `expires_at` BACKWARD (without expiring it) and
    // re-authenticate — the expiry should bounce back to the
    // freshly-extended value.
    let repo: Arc<InMemorySessionRepo> = Arc::new(InMemorySessionRepo::default());
    let svc = SessionService::new(repo.clone() as Arc<dyn SessionRepo>, vec![7u8; 32])
        .unwrap()
        .with_idle_ttl(Duration::from_secs(60))
        .with_absolute_max(Duration::from_secs(3600));
    let issued = svc.issue(fresh_user_id(), None, None).await.unwrap();
    let original_expiry = issued.expires_at;

    // Walk the row's `expires_at` backward to ~now+10s. Next
    // authenticate should jump it forward to ~now+60s.
    {
        let mut inner = repo.inner.lock().unwrap();
        let row = inner.values_mut().next().unwrap();
        row.expires_at = Utc::now() + chrono::Duration::seconds(10);
    }
    let validated = svc
        .authenticate(&issued.cookie_value)
        .await
        .unwrap()
        .expect("session still valid");
    let walked_window_max = Utc::now() + chrono::Duration::seconds(15);
    assert!(
        validated.expires_at > walked_window_max,
        "expires_at must slide forward, got {} (walked + 5s = {walked_window_max})",
        validated.expires_at
    );
    // The persisted row's expiry matches what the service
    // returned (sliding kept it < absolute_expires_at).
    let inner = repo.inner.lock().unwrap();
    let row = inner.values().next().unwrap();
    assert_eq!(row.expires_at, validated.expires_at);
    assert!(
        validated.expires_at <= row.absolute_expires_at,
        "slide must never exceed absolute cap"
    );

    // `original_expiry` is from `Utc::now()` taken at issue
    // time; the slide ran at a strictly later `Utc::now()`, so
    // the values differ.
    drop(inner);
    assert!(
        validated.expires_at > original_expiry,
        "slide must advance past the original idle expiry"
    );
}

#[tokio::test]
async fn authenticate_slide_is_capped_at_absolute_max() {
    // Regression for Copilot R2 (absolute cap): with idle_ttl >
    // absolute_max, the slide must be CLAMPED to
    // absolute_expires_at — an actively-used session can't live
    // past the hard cap.
    let repo: Arc<InMemorySessionRepo> = Arc::new(InMemorySessionRepo::default());
    let svc = SessionService::new(repo.clone() as Arc<dyn SessionRepo>, vec![7u8; 32])
        .unwrap()
        // Absurdly large idle, tiny absolute — the slide value
        // is enormous but the cap pins `expires_at` to
        // absolute_expires_at.
        .with_idle_ttl(Duration::from_secs(3600))
        .with_absolute_max(Duration::from_secs(60));
    let issued = svc.issue(fresh_user_id(), None, None).await.unwrap();
    let abs = {
        let inner = repo.inner.lock().unwrap();
        inner.values().next().unwrap().absolute_expires_at
    };

    let validated = svc
        .authenticate(&issued.cookie_value)
        .await
        .unwrap()
        .expect("session still valid");
    assert_eq!(
        validated.expires_at, abs,
        "slide must clamp to absolute_expires_at when idle_ttl > absolute_max"
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
        // Cookie value is `<token>.<tag>`. The DB key is
        // sha256(<raw token bytes>), so peel off the tag before
        // hashing.
        let (token_b64, _) = dead.cookie_value.split_once('.').unwrap();
        let bytes = URL_SAFE_NO_PAD.decode(token_b64).unwrap();
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

// --- retry-on-collision fake ----------------------------------------

/// Wraps a real session repo and rejects the first N
/// `insert_session` calls with `UniqueViolation`, then defers to
/// the inner repo. Drives `SessionService::issue` down the retry
/// path without needing `OsRng` to actually collide.
struct CollidingRepo {
    inner: Arc<InMemorySessionRepo>,
    reject_until_attempt: Mutex<u32>,
    attempts: Mutex<u32>,
}

#[async_trait]
impl SessionRepo for CollidingRepo {
    async fn insert_session(&self, new: NewSession) -> Result<(), StorageError> {
        // Walk the counter + collision check inside a tight
        // scope so neither MutexGuard crosses the await below.
        let collide = {
            let mut attempts = self.attempts.lock().unwrap();
            *attempts += 1;
            let limit = *self.reject_until_attempt.lock().unwrap();
            *attempts <= limit
        };
        if collide {
            return Err(StorageError::UniqueViolation("sessions_pkey".to_owned()));
        }
        self.inner.insert_session(new).await
    }
    async fn touch_and_authenticate(
        &self,
        id_hash: &[u8],
        now: DateTime<Utc>,
        new_expires_at: DateTime<Utc>,
    ) -> Result<Option<AuthenticatedSession>, StorageError> {
        self.inner
            .touch_and_authenticate(id_hash, now, new_expires_at)
            .await
    }
    async fn revoke_session(
        &self,
        id_hash: &[u8],
        now: DateTime<Utc>,
    ) -> Result<bool, StorageError> {
        self.inner.revoke_session(id_hash, now).await
    }
    async fn revoke_all_sessions_for_user(
        &self,
        user_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<u64, StorageError> {
        self.inner.revoke_all_sessions_for_user(user_id, now).await
    }
}

#[tokio::test]
async fn authenticate_slide_is_monotonic_under_clock_skew() {
    // Regression for Copilot R4 (monotonic slide): if a request
    // computes `new_expires_at` that's SMALLER than the row's
    // current `expires_at` (concurrent requests + skewed clocks
    // can produce this), the slide must NOT shrink the expiry —
    // it should leave the larger value in place.
    let repo: Arc<InMemorySessionRepo> = Arc::new(InMemorySessionRepo::default());
    let svc = SessionService::new(repo.clone() as Arc<dyn SessionRepo>, vec![7u8; 32])
        .unwrap()
        .with_idle_ttl(Duration::from_secs(60))
        .with_absolute_max(Duration::from_secs(3600));
    let issued = svc.issue(fresh_user_id(), None, None).await.unwrap();

    // Walk the row's `expires_at` FORWARD to ~now + 50s (still
    // under the absolute cap). Next authenticate computes
    // `new_expires_at = now + idle_ttl(60s)`. Without
    // monotonicity, the LEAST clamp would still pick the
    // smaller of the two — but with GREATEST in the SQL we
    // expect the row's expires_at to stay at the higher value
    // when the slide would otherwise shrink it.
    {
        let mut inner = repo.inner.lock().unwrap();
        let row = inner.values_mut().next().unwrap();
        row.expires_at = Utc::now() + chrono::Duration::seconds(50);
    }
    let prior_expiry = repo
        .inner
        .lock()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .expires_at;
    let validated = svc
        .authenticate(&issued.cookie_value)
        .await
        .unwrap()
        .unwrap();
    assert!(
        validated.expires_at >= prior_expiry,
        "slide must be monotonic: post-touch >= prior expires_at ({prior_expiry} > {})",
        validated.expires_at
    );
}

#[tokio::test]
#[should_panic(expected = "idle_ttl must be >= 1s")]
async fn with_idle_ttl_panics_on_sub_second() {
    // Sub-second idle TTLs would `as_secs()`-truncate to 0 and
    // emit `Max-Age=0`, which browsers interpret as immediate
    // cookie deletion. Catch at startup.
    let repo: Arc<InMemorySessionRepo> = Arc::new(InMemorySessionRepo::default());
    let _ = SessionService::new(repo as Arc<dyn SessionRepo>, vec![7u8; 32])
        .unwrap()
        .with_idle_ttl(Duration::from_millis(500));
}

#[tokio::test]
async fn cookie_max_age_seconds_is_never_zero_for_valid_config() {
    // With the assert in `with_absolute_max`, the lowest legal
    // `absolute_max` is 1s, which maps to `Max-Age=1`. Confirms
    // the gateway can never emit `Max-Age=0` via this path.
    let repo: Arc<InMemorySessionRepo> = Arc::new(InMemorySessionRepo::default());
    let svc = SessionService::new(repo as Arc<dyn SessionRepo>, vec![7u8; 32])
        .unwrap()
        .with_idle_ttl(Duration::from_secs(1))
        .with_absolute_max(Duration::from_secs(1));
    assert_eq!(svc.cookie_max_age_seconds(), 1);
}

#[tokio::test]
async fn issue_retries_on_pk_collision() {
    // Regression for Copilot R3: `issue()` retries on
    // `UniqueViolation`. The fake rejects the first attempt, so
    // the second attempt (with a fresh OsRng token) succeeds —
    // the caller sees a single Ok return.
    let inner = Arc::new(InMemorySessionRepo::default());
    let repo = Arc::new(CollidingRepo {
        inner,
        reject_until_attempt: Mutex::new(1),
        attempts: Mutex::new(0),
    });
    let svc = SessionService::new(repo.clone() as Arc<dyn SessionRepo>, vec![7u8; 32])
        .unwrap()
        .with_idle_ttl(Duration::from_secs(60))
        .with_absolute_max(Duration::from_secs(60));
    let issued = svc.issue(fresh_user_id(), None, None).await.unwrap();
    assert_eq!(
        *repo.attempts.lock().unwrap(),
        2,
        "first attempt collides, second succeeds"
    );
    // Session landed in the inner repo.
    assert!(
        svc.authenticate(&issued.cookie_value)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn issue_surfaces_internal_error_after_max_collisions() {
    // Regression for Copilot R3 (defective-RNG case): every
    // attempt collides → `issue()` returns
    // `AuthError::Internal` rather than spinning forever.
    let inner = Arc::new(InMemorySessionRepo::default());
    let repo = Arc::new(CollidingRepo {
        inner,
        reject_until_attempt: Mutex::new(u32::MAX),
        attempts: Mutex::new(0),
    });
    let svc = SessionService::new(repo.clone() as Arc<dyn SessionRepo>, vec![7u8; 32])
        .unwrap()
        .with_idle_ttl(Duration::from_secs(60))
        .with_absolute_max(Duration::from_secs(60));
    let err = svc.issue(fresh_user_id(), None, None).await.unwrap_err();
    assert!(matches!(&err, auth::AuthError::Internal(msg) if msg.contains("PK collision")));
    // Three attempts, then surface the error.
    assert_eq!(*repo.attempts.lock().unwrap(), 3);
}
