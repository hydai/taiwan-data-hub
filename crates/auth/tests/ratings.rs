//! Integration tests for [`auth::RatingService`] (#5a.5).
//!
//! Uses an in-memory [`storage::RatingRepo`] fake that
//! mirrors the production behaviour: idempotent upsert,
//! aggregate refresh on every write, anonymous-readable
//! view. A minimal [`storage::UserRepo`] fake supplies the
//! `created_at` the service consults for the 24h anti-spam
//! gate.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use auth::{MIN_ACCOUNT_AGE_FOR_RATING, RatingDenialReason, RatingService};
use chrono::{DateTime, Duration, Utc};
use storage::{
    RatingAggregateRow, RatingRepo, RatingRow, RatingTargetKind, RatingView, StorageError, User,
    UserRepo, UserRole,
};
use uuid::Uuid;

// === Rating fake ===

#[derive(Default)]
struct RatingStore {
    rows: Mutex<HashMap<Uuid, RatingRow>>,
    // Aggregate rows persist across writes — mirroring the
    // production `rating_aggregates` table where a withdrawal
    // that takes the count to 0 leaves the row behind with
    // `rating_count = 0, avg_score = 0` (the gateway treats
    // that identically to a missing row).
    aggregates: Mutex<HashMap<(RatingTargetKind, Uuid), RatingAggregateRow>>,
}

/// Recompute the aggregate from the score rows. Returns
/// `Some(rating_count = 0)` when the target has had ratings
/// before but they've all been withdrawn — mirroring the
/// production `INSERT ... ON CONFLICT DO UPDATE` shape.
fn aggregate_from_scores(
    rows: &HashMap<Uuid, RatingRow>,
    target_kind: RatingTargetKind,
    target_id: Uuid,
    now: DateTime<Utc>,
) -> RatingAggregateRow {
    let scores: Vec<i16> = rows
        .values()
        .filter(|r| r.target_kind == target_kind && r.target_id == target_id)
        .map(|r| r.score)
        .collect();
    let count = i32::try_from(scores.len()).unwrap_or(i32::MAX);
    let avg = if scores.is_empty() {
        0.0
    } else {
        let sum: f64 = scores.iter().copied().map(f64::from).sum();
        sum / f64::from(count)
    };
    RatingAggregateRow {
        target_kind,
        target_id,
        avg_score: avg,
        rating_count: count,
        last_refreshed_at: now,
    }
}

impl RatingStore {
    /// Refresh the persisted aggregate from the current score
    /// rows. Always upserts (writes a `count == 0` row when
    /// nothing remains) — same shape as production's
    /// `INSERT ... ON CONFLICT DO UPDATE`.
    fn refresh_aggregate(
        &self,
        rows: &HashMap<Uuid, RatingRow>,
        target_kind: RatingTargetKind,
        target_id: Uuid,
        now: DateTime<Utc>,
    ) {
        let fresh = aggregate_from_scores(rows, target_kind, target_id, now);
        self.aggregates
            .lock()
            .unwrap()
            .insert((target_kind, target_id), fresh);
    }
}

#[async_trait]
impl RatingRepo for RatingStore {
    async fn upsert(
        &self,
        user_id: Uuid,
        target_kind: RatingTargetKind,
        target_id: Uuid,
        score: i16,
        now: DateTime<Utc>,
    ) -> Result<RatingRow, StorageError> {
        let row = {
            let mut inner = self.rows.lock().unwrap();
            let existing = inner
                .iter()
                .find(|(_, r)| {
                    r.user_id == user_id && r.target_kind == target_kind && r.target_id == target_id
                })
                .map(|(id, r)| (*id, r.created_at));
            if let Some((id, created_at)) = existing {
                let updated = RatingRow {
                    id,
                    user_id,
                    target_kind,
                    target_id,
                    score,
                    created_at,
                    updated_at: now,
                };
                inner.insert(id, updated.clone());
                updated
            } else {
                let id = Uuid::now_v7();
                let fresh = RatingRow {
                    id,
                    user_id,
                    target_kind,
                    target_id,
                    score,
                    created_at: now,
                    updated_at: now,
                };
                inner.insert(id, fresh.clone());
                fresh
            }
        };
        let snapshot = self.rows.lock().unwrap().clone();
        self.refresh_aggregate(&snapshot, target_kind, target_id, now);
        Ok(row)
    }

    async fn withdraw(
        &self,
        user_id: Uuid,
        target_kind: RatingTargetKind,
        target_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<bool, StorageError> {
        let removed = {
            let mut inner = self.rows.lock().unwrap();
            let target = inner
                .iter()
                .find(|(_, r)| {
                    r.user_id == user_id && r.target_kind == target_kind && r.target_id == target_id
                })
                .map(|(id, _)| *id);
            target.is_some_and(|id| inner.remove(&id).is_some())
        };
        let snapshot = self.rows.lock().unwrap().clone();
        self.refresh_aggregate(&snapshot, target_kind, target_id, now);
        Ok(removed)
    }

    async fn view(
        &self,
        target_kind: RatingTargetKind,
        target_id: Uuid,
        viewer_id: Option<Uuid>,
    ) -> Result<RatingView, StorageError> {
        let aggregate = self
            .aggregates
            .lock()
            .unwrap()
            .get(&(target_kind, target_id))
            .cloned();
        let inner = self.rows.lock().unwrap();
        let viewer_score = viewer_id.and_then(|uid| {
            inner
                .values()
                .find(|r| {
                    r.user_id == uid && r.target_kind == target_kind && r.target_id == target_id
                })
                .map(|r| r.score)
        });
        Ok(RatingView {
            aggregate,
            viewer_score,
        })
    }

    async fn aggregates_for(
        &self,
        targets: &[(RatingTargetKind, Uuid)],
    ) -> Result<Vec<RatingAggregateRow>, StorageError> {
        let inner = self.aggregates.lock().unwrap();
        let mut out = Vec::new();
        for (kind, id) in targets {
            if let Some(agg) = inner.get(&(*kind, *id))
                && agg.rating_count > 0
            {
                out.push(agg.clone());
            }
        }
        Ok(out)
    }
}

// === User fake (only the methods RatingService consults) ===

#[derive(Default)]
struct UserStore {
    rows: Mutex<HashMap<Uuid, User>>,
}

impl UserStore {
    fn insert(&self, id: Uuid, created_at: DateTime<Utc>) {
        self.rows.lock().unwrap().insert(
            id,
            User {
                id,
                email: format!("{id}@test"),
                password_hash: String::new(),
                email_verified_at: Some(created_at),
                role: UserRole::User,
                created_at,
                updated_at: created_at,
            },
        );
    }
}

#[async_trait]
impl UserRepo for UserStore {
    async fn insert_user(&self, _email: &str, _hash: &str) -> Result<User, StorageError> {
        unreachable!("not used by rating tests")
    }
    async fn find_user_by_email(&self, _email: &str) -> Result<Option<User>, StorageError> {
        unreachable!("not used by rating tests")
    }
    async fn find_user_by_id(&self, id: Uuid) -> Result<Option<User>, StorageError> {
        Ok(self.rows.lock().unwrap().get(&id).cloned())
    }
    async fn find_user_role(&self, _id: Uuid) -> Result<Option<UserRole>, StorageError> {
        unreachable!("not used by rating tests")
    }
    async fn mark_email_verified(&self, _id: Uuid) -> Result<bool, StorageError> {
        unreachable!("not used by rating tests")
    }
    async fn update_password_hash(&self, _id: Uuid, _hash: &str) -> Result<bool, StorageError> {
        unreachable!("not used by rating tests")
    }
    async fn delete_user(&self, _id: Uuid) -> Result<bool, StorageError> {
        unreachable!("not used by rating tests")
    }
}

// === Helpers ===

fn make_service(users: &Arc<UserStore>) -> (Arc<RatingStore>, RatingService) {
    let ratings = Arc::new(RatingStore::default());
    let svc = RatingService::new(
        ratings.clone() as Arc<dyn RatingRepo>,
        users.clone() as Arc<dyn UserRepo>,
    );
    (ratings, svc)
}

fn established_user(users: &UserStore) -> Uuid {
    let id = Uuid::now_v7();
    // 7 days old — well past the 24h gate.
    users.insert(id, Utc::now() - Duration::days(7));
    id
}

fn fresh_user(users: &UserStore) -> Uuid {
    let id = Uuid::now_v7();
    users.insert(id, Utc::now() - Duration::minutes(1));
    id
}

// === Tests ===

#[tokio::test]
async fn rejects_score_below_min() {
    let users = Arc::new(UserStore::default());
    let user = established_user(&users);
    let (_repo, svc) = make_service(&users);
    let target = Uuid::now_v7();
    let outcome = svc
        .upsert(user, RatingTargetKind::Dataset, target, 0)
        .await
        .unwrap();
    assert_eq!(outcome.unwrap_err(), RatingDenialReason::ScoreOutOfRange);
}

#[tokio::test]
async fn rejects_score_above_max() {
    let users = Arc::new(UserStore::default());
    let user = established_user(&users);
    let (_repo, svc) = make_service(&users);
    let target = Uuid::now_v7();
    let outcome = svc
        .upsert(user, RatingTargetKind::Dataset, target, 6)
        .await
        .unwrap();
    assert_eq!(outcome.unwrap_err(), RatingDenialReason::ScoreOutOfRange);
}

#[tokio::test]
async fn rejects_too_young_account() {
    let users = Arc::new(UserStore::default());
    let user = fresh_user(&users);
    let (_repo, svc) = make_service(&users);
    let target = Uuid::now_v7();
    let outcome = svc
        .upsert(user, RatingTargetKind::Dataset, target, 4)
        .await
        .unwrap();
    assert_eq!(outcome.unwrap_err(), RatingDenialReason::AccountTooNew);
}

#[tokio::test]
async fn rejects_unknown_user() {
    let users = Arc::new(UserStore::default());
    let (_repo, svc) = make_service(&users);
    let target = Uuid::now_v7();
    // No user inserted — should land in UnknownUser.
    let outcome = svc
        .upsert(Uuid::now_v7(), RatingTargetKind::Dataset, target, 3)
        .await
        .unwrap();
    assert_eq!(outcome.unwrap_err(), RatingDenialReason::UnknownUser);
}

#[tokio::test]
async fn upsert_round_trips() {
    let users = Arc::new(UserStore::default());
    let user = established_user(&users);
    let (_repo, svc) = make_service(&users);
    let target = Uuid::now_v7();
    let row = svc
        .upsert(user, RatingTargetKind::Dataset, target, 5)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.score, 5);
    let view = svc
        .view(RatingTargetKind::Dataset, target, Some(user))
        .await
        .unwrap();
    assert_eq!(view.viewer_score, Some(5));
    let agg = view.aggregate.unwrap();
    assert_eq!(agg.rating_count, 1);
    assert!((agg.avg_score - 5.0).abs() < f64::EPSILON);
}

#[tokio::test]
async fn re_rating_overwrites_score() {
    let users = Arc::new(UserStore::default());
    let user = established_user(&users);
    let (_repo, svc) = make_service(&users);
    let target = Uuid::now_v7();
    svc.upsert(user, RatingTargetKind::Dataset, target, 3)
        .await
        .unwrap()
        .unwrap();
    let updated = svc
        .upsert(user, RatingTargetKind::Dataset, target, 4)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.score, 4);
    let view = svc
        .view(RatingTargetKind::Dataset, target, Some(user))
        .await
        .unwrap();
    assert_eq!(view.aggregate.unwrap().rating_count, 1);
    assert_eq!(view.viewer_score, Some(4));
}

#[tokio::test]
async fn withdraw_drops_row_and_aggregate() {
    let users = Arc::new(UserStore::default());
    let user = established_user(&users);
    let (_repo, svc) = make_service(&users);
    let target = Uuid::now_v7();
    svc.upsert(user, RatingTargetKind::Dataset, target, 2)
        .await
        .unwrap()
        .unwrap();
    let removed = svc
        .withdraw(user, RatingTargetKind::Dataset, target)
        .await
        .unwrap();
    assert!(removed);
    let view = svc
        .view(RatingTargetKind::Dataset, target, Some(user))
        .await
        .unwrap();
    // Production keeps the `rating_aggregates` row around
    // with `rating_count = 0` after the last withdrawal —
    // the on-write refresh upserts unconditionally. Gateway
    // treats `count == 0` identically to "no row".
    let agg = view
        .aggregate
        .expect("aggregate should persist with count=0");
    assert_eq!(agg.rating_count, 0);
    assert!((agg.avg_score - 0.0).abs() < f64::EPSILON);
    assert_eq!(view.viewer_score, None);
    // Idempotent second withdraw.
    assert!(
        !svc.withdraw(user, RatingTargetKind::Dataset, target)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn anonymous_view_returns_aggregate_only() {
    let users = Arc::new(UserStore::default());
    let alice = established_user(&users);
    let bob = established_user(&users);
    let (_repo, svc) = make_service(&users);
    let target = Uuid::now_v7();
    svc.upsert(alice, RatingTargetKind::Dataset, target, 4)
        .await
        .unwrap()
        .unwrap();
    svc.upsert(bob, RatingTargetKind::Dataset, target, 2)
        .await
        .unwrap()
        .unwrap();
    let view = svc
        .view(RatingTargetKind::Dataset, target, None)
        .await
        .unwrap();
    assert_eq!(view.viewer_score, None);
    let agg = view.aggregate.unwrap();
    assert_eq!(agg.rating_count, 2);
    assert!((agg.avg_score - 3.0).abs() < f64::EPSILON);
}

#[test]
fn min_account_age_is_one_day() {
    // Pin the threshold so a tweak shows up in CI rather
    // than silently changing the anti-spam policy.
    assert_eq!(MIN_ACCOUNT_AGE_FOR_RATING, Duration::hours(24));
}
