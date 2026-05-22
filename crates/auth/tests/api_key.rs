//! Integration tests for [`auth::ApiKeyService`] (#4.6).
//!
//! Uses an in-memory [`storage::ApiKeyRepo`] fake that mirrors
//! the production SQL semantics exactly: PK-collision raises
//! `UniqueViolation`, `touch_and_verify` filters revoked rows
//! and clamps `last_used_at` via `max`, `revoke` is idempotent
//! and ownership-scoped. Anything that drifts from this fake
//! would let a regression hide between the SQL and the auth
//! crate — so the fake is the spec.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use auth::{ALLOWED_TIERS, API_KEY_HUMAN_PREFIX, ApiKeyService, AuthError};
use chrono::{DateTime, Utc};
use storage::{ApiKeyRepo, ApiKeyRow, NewApiKey, StorageError};
use uuid::Uuid;

#[derive(Default)]
struct InMemoryApiKeyRepo {
    inner: Mutex<HashMap<Uuid, Row>>,
    /// Optional hook that forces the first N `insert_api_key`
    /// calls to fail with `UniqueViolation`. Lets the retry test
    /// drive the loop deterministically.
    force_pk_collisions: Mutex<u32>,
}

#[derive(Clone)]
struct Row {
    id: Uuid,
    user_id: Uuid,
    name: String,
    key_prefix: String,
    key_hash: Vec<u8>,
    scopes: Vec<String>,
    rate_limit_tier: String,
    created_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
}

impl InMemoryApiKeyRepo {
    fn force_collisions(&self, n: u32) {
        *self.force_pk_collisions.lock().unwrap() = n;
    }

    fn rows_by_hash(&self, key_hash: &[u8]) -> Option<Row> {
        self.inner
            .lock()
            .unwrap()
            .values()
            .find(|r| r.key_hash == key_hash)
            .cloned()
    }
}

#[async_trait]
impl ApiKeyRepo for InMemoryApiKeyRepo {
    async fn insert_api_key(&self, new: NewApiKey) -> Result<Uuid, StorageError> {
        // Pop a collision token if one is queued.
        {
            let mut remaining = self.force_pk_collisions.lock().unwrap();
            if *remaining > 0 {
                *remaining -= 1;
                return Err(StorageError::UniqueViolation(
                    "mcp_api_keys_key_hash_idx".into(),
                ));
            }
        }
        // Real DB enforces UNIQUE on `key_hash` via the unique
        // index `mcp_api_keys_key_hash_idx` (R1 made it UNIQUE
        // across ALL rows, not partial). Mirror that here so a
        // duplicated hash from a (very unlikely) collision DOES
        // surface as `UniqueViolation` even outside the forced
        // path.
        {
            let inner = self.inner.lock().unwrap();
            if inner.values().any(|r| r.key_hash == new.key_hash) {
                return Err(StorageError::UniqueViolation(
                    "mcp_api_keys_key_hash_idx".into(),
                ));
            }
        }
        let id = Uuid::now_v7();
        // Mirror the production INSERT: `created_at` comes from
        // `new.created_at` (the caller-supplied `now`), NOT a
        // fresh `Utc::now()`. The fake's row keeps the same
        // single-clock-source invariant as the real table —
        // exercising the production audit-timeline guarantee
        // even in tests.
        let row = Row {
            id,
            user_id: new.user_id,
            name: new.name,
            key_prefix: new.key_prefix,
            key_hash: new.key_hash,
            scopes: new.scopes,
            rate_limit_tier: new.rate_limit_tier,
            created_at: new.created_at,
            last_used_at: None,
            revoked_at: None,
        };
        self.inner.lock().unwrap().insert(id, row);
        Ok(id)
    }

    async fn touch_and_verify(
        &self,
        key_hash: &[u8],
        now: DateTime<Utc>,
    ) -> Result<Option<ApiKeyRow>, StorageError> {
        let mut inner = self.inner.lock().unwrap();
        // Find the row by hash with linear scan — fine for tests;
        // production uses the unique index on `key_hash`
        // (`mcp_api_keys_key_hash_idx`).
        let Some(row) = inner.values_mut().find(|r| r.key_hash == key_hash) else {
            return Ok(None);
        };
        if row.revoked_at.is_some() {
            return Ok(None);
        }
        // Mirror the production `GREATEST(COALESCE(...), $2)`:
        // last_used_at never moves backwards even under clock
        // skew.
        let next = match row.last_used_at {
            Some(prev) if prev > now => prev,
            _ => now,
        };
        row.last_used_at = Some(next);
        Ok(Some(snapshot(row)))
    }

    async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<ApiKeyRow>, StorageError> {
        let inner = self.inner.lock().unwrap();
        let mut rows: Vec<ApiKeyRow> = inner
            .values()
            .filter(|r| r.user_id == user_id)
            .map(snapshot)
            .collect();
        // Mirror the production `ORDER BY created_at DESC`. Clippy
        // wants `sort_by_key`, but the natural key is `Reverse
        // (created_at)` — go that way for a clean descending sort
        // without an inline closure.
        rows.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        Ok(rows)
    }

    async fn revoke(
        &self,
        id: Uuid,
        user_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<Option<ApiKeyRow>, StorageError> {
        let mut inner = self.inner.lock().unwrap();
        let Some(row) = inner.get_mut(&id) else {
            return Ok(None);
        };
        if row.user_id != user_id || row.revoked_at.is_some() {
            return Ok(None);
        }
        row.revoked_at = Some(now);
        Ok(Some(snapshot(row)))
    }
}

fn snapshot(row: &Row) -> ApiKeyRow {
    ApiKeyRow {
        id: row.id,
        user_id: row.user_id,
        name: row.name.clone(),
        key_prefix: row.key_prefix.clone(),
        scopes: row.scopes.clone(),
        rate_limit_tier: row.rate_limit_tier.clone(),
        created_at: row.created_at,
        last_used_at: row.last_used_at,
        revoked_at: row.revoked_at,
    }
}

fn build_service() -> (ApiKeyService, Arc<InMemoryApiKeyRepo>) {
    let repo: Arc<InMemoryApiKeyRepo> = Arc::new(InMemoryApiKeyRepo::default());
    let svc = ApiKeyService::new(repo.clone() as Arc<dyn ApiKeyRepo>);
    (svc, repo)
}

fn fresh_user_id() -> Uuid {
    Uuid::now_v7()
}

// --- tests ----------------------------------------------------------

#[tokio::test]
async fn issue_then_verify_round_trip() {
    let (svc, _repo) = build_service();
    let user = fresh_user_id();
    let issued = svc
        .issue(user, "laptop".into(), vec![], "free".into())
        .await
        .expect("issue ok");
    assert!(issued.cleartext.starts_with(API_KEY_HUMAN_PREFIX));
    assert!(issued.key_prefix.starts_with(API_KEY_HUMAN_PREFIX));

    let verified = svc
        .verify(&issued.cleartext)
        .await
        .expect("verify ok")
        .expect("Some");
    assert_eq!(verified.id, issued.id);
    assert_eq!(verified.user_id, user);
    assert_eq!(verified.rate_limit_tier, "free");
}

#[tokio::test]
async fn verify_returns_none_for_malformed_key() {
    let (svc, _) = build_service();
    // Wrong prefix.
    assert!(svc.verify("nope_abcdef").await.unwrap().is_none());
    // Right prefix, wrong length.
    assert!(svc.verify("tdh_short").await.unwrap().is_none());
    // Right shape, but never issued — collapses to None at the
    // DB lookup, NOT a typed error.
    let synthetic = format!("{API_KEY_HUMAN_PREFIX}{}", "a".repeat(43));
    assert!(svc.verify(&synthetic).await.unwrap().is_none());
}

#[tokio::test]
async fn verify_returns_none_after_revoke() {
    let (svc, _) = build_service();
    let user = fresh_user_id();
    let issued = svc
        .issue(user, "rotated".into(), vec![], "free".into())
        .await
        .unwrap();
    svc.revoke(issued.id, user).await.unwrap().expect("revoked");
    assert!(svc.verify(&issued.cleartext).await.unwrap().is_none());
}

#[tokio::test]
async fn revoke_is_idempotent_and_owner_scoped() {
    let (svc, _) = build_service();
    let user = fresh_user_id();
    let other = fresh_user_id();
    let issued = svc
        .issue(user, "test".into(), vec![], "free".into())
        .await
        .unwrap();
    // Owner can revoke once.
    assert!(svc.revoke(issued.id, user).await.unwrap().is_some());
    // Second revoke by owner — already revoked, return None.
    assert!(svc.revoke(issued.id, user).await.unwrap().is_none());
    // Other user's revoke is None even on a never-revoked key —
    // (we use a fresh key to assert the ownership check fires).
    let issued2 = svc
        .issue(user, "test2".into(), vec![], "free".into())
        .await
        .unwrap();
    assert!(svc.revoke(issued2.id, other).await.unwrap().is_none());
    // …and the key is still verifiable by its owner — the
    // wrong-user revoke did not actually flip the row.
    assert!(svc.verify(&issued2.cleartext).await.unwrap().is_some());
}

#[tokio::test]
async fn list_for_user_returns_keys_newest_first() {
    let (svc, _) = build_service();
    let user = fresh_user_id();
    let first = svc
        .issue(user, "first".into(), vec![], "free".into())
        .await
        .unwrap();
    // Tiny sleep so created_at is strictly later for the second
    // row — Utc::now() resolution is sub-microsecond but the
    // fake captures real wall-clock at insert, so guarantee
    // ordering with a yield.
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let second = svc
        .issue(user, "second".into(), vec![], "free".into())
        .await
        .unwrap();
    let rows = svc.list_for_user(user).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, second.id, "newest first");
    assert_eq!(rows[1].id, first.id);
}

#[tokio::test]
async fn issue_validates_tier() {
    let (svc, _) = build_service();
    let err = svc
        .issue(fresh_user_id(), "test".into(), vec![], "godmode".into())
        .await
        .unwrap_err();
    match err {
        AuthError::Validation(m) => {
            assert!(m.contains("godmode"), "msg={m}");
        }
        other => panic!("expected Validation, got {other:?}"),
    }
    // Sanity: every nominal tier is accepted.
    for tier in ALLOWED_TIERS {
        svc.issue(
            fresh_user_id(),
            "test".into(),
            vec![],
            (*tier).to_owned(),
        )
        .await
        .unwrap_or_else(|e| panic!("tier {tier} should pass: {e:?}"));
    }
}

#[tokio::test]
async fn issue_validates_non_empty_name() {
    let (svc, _) = build_service();
    let err = svc
        .issue(fresh_user_id(), "   ".into(), vec![], "free".into())
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::Validation(_)), "got {err:?}");
}

#[tokio::test]
async fn issue_retries_on_pk_collision() {
    let (svc, repo) = build_service();
    repo.force_collisions(1);
    let issued = svc
        .issue(fresh_user_id(), "retry".into(), vec![], "free".into())
        .await
        .expect("retried successfully");
    // The row that finally landed has a fresh entropy — verify
    // we can authenticate against it.
    assert!(svc.verify(&issued.cleartext).await.unwrap().is_some());
}

#[tokio::test]
async fn issue_surfaces_internal_after_max_collisions() {
    let (svc, repo) = build_service();
    // ISSUE_MAX_ATTEMPTS = 3 in the auth crate; force ≥ that
    // many collisions so the loop exits the retry path.
    repo.force_collisions(3);
    let err = svc
        .issue(fresh_user_id(), "always-collides".into(), vec![], "free".into())
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::Internal(_)), "got {err:?}");
}

#[tokio::test]
async fn rotate_revokes_old_and_returns_new() {
    let (svc, repo) = build_service();
    let user = fresh_user_id();
    let original = svc
        .issue(user, "ci".into(), vec!["read".into()], "pro".into())
        .await
        .unwrap();
    let rotated = svc
        .rotate(original.id, user)
        .await
        .unwrap()
        .expect("Some");
    // New key carries the same metadata as the old.
    let listed = svc.list_for_user(user).await.unwrap();
    let new_row = listed.iter().find(|r| r.id == rotated.id).unwrap();
    assert_eq!(new_row.name, "ci");
    assert_eq!(new_row.scopes, vec!["read"]);
    assert_eq!(new_row.rate_limit_tier, "pro");
    assert!(new_row.revoked_at.is_none());
    // Old key is revoked and no longer verifies.
    assert!(svc.verify(&original.cleartext).await.unwrap().is_none());
    let _ = repo.rows_by_hash(b"unused"); // exercise helper
}

#[tokio::test]
async fn rotate_returns_none_for_unknown_or_already_revoked() {
    let (svc, _) = build_service();
    let user = fresh_user_id();
    // Unknown id — no row to revoke, returns None.
    assert!(svc.rotate(Uuid::now_v7(), user).await.unwrap().is_none());

    // Already-revoked id — first rotate returns Some, second
    // returns None (idempotent on the source row).
    let issued = svc
        .issue(user, "rot".into(), vec![], "free".into())
        .await
        .unwrap();
    assert!(svc.rotate(issued.id, user).await.unwrap().is_some());
    assert!(svc.rotate(issued.id, user).await.unwrap().is_none());
}

#[tokio::test]
async fn issue_normalises_scopes_at_the_service_layer() {
    // The web form already trims + drops empties before
    // POSTing, but the auth service is the canonical caller-
    // agnostic surface. `issue` MUST normalise so future MCP
    // / CLI / batch clients (which don't run the web's form
    // logic) get the same row shape: trimmed, no empties,
    // sorted, no duplicates.
    let (svc, _) = build_service();
    let user = fresh_user_id();
    let issued = svc
        .issue(
            user,
            "messy".into(),
            vec![
                "  read  ".into(),
                "write".into(),
                String::new(),
                "   ".into(),
                "read".into(), // duplicate, will be deduped
            ],
            "free".into(),
        )
        .await
        .expect("issue ok");

    let rows = svc.list_for_user(user).await.unwrap();
    let persisted = rows.iter().find(|r| r.id == issued.id).unwrap();
    assert_eq!(
        persisted.scopes,
        vec!["read".to_owned(), "write".to_owned()],
        "normalisation should trim, drop empties, dedup, and sort"
    );
}

#[tokio::test]
async fn verify_touches_last_used_at_monotonically() {
    let (svc, repo) = build_service();
    let user = fresh_user_id();
    let issued = svc
        .issue(user, "touch".into(), vec![], "free".into())
        .await
        .unwrap();
    // First verify sets last_used_at; second verify must not
    // shrink it (mirrors the GREATEST clamp).
    let _ = svc.verify(&issued.cleartext).await.unwrap();
    let row1 = svc.list_for_user(user).await.unwrap();
    let first_touch = row1[0].last_used_at.expect("touched once");
    // Wait a tick to make wall-clock progress observable.
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let _ = svc.verify(&issued.cleartext).await.unwrap();
    let row2 = svc.list_for_user(user).await.unwrap();
    let second_touch = row2[0].last_used_at.expect("touched again");
    assert!(
        second_touch >= first_touch,
        "second={second_touch}, first={first_touch}"
    );
    // Manually push the row's last_used_at into the future and
    // confirm the next verify does NOT roll it back.
    {
        let mut inner = repo.inner.lock().unwrap();
        let row = inner.get_mut(&issued.id).unwrap();
        row.last_used_at = Some(second_touch + chrono::Duration::hours(1));
    }
    let _ = svc.verify(&issued.cleartext).await.unwrap();
    let row3 = svc.list_for_user(user).await.unwrap();
    assert!(
        row3[0].last_used_at.unwrap() >= second_touch + chrono::Duration::hours(1),
        "GREATEST kept the future timestamp"
    );
}
