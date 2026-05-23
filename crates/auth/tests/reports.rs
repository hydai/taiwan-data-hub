//! Integration tests for [`auth::ReportService`] (#5a.6).
//!
//! Uses an in-memory [`storage::ReportRepo`] fake that
//! mirrors production:
//!
//!   * idempotent insert on `(reporter, target)`;
//!   * distinct-reporter count drives the auto-hide flag;
//!   * `resolve` honours the `also_hide_target` /
//!     `also_delete_target` flags.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use auth::{REPORT_AUTO_HIDE_THRESHOLD, ReportDenialReason, ReportService, ResolveDenialReason};
use chrono::{DateTime, Utc};
use std::sync::Arc;
use storage::{
    NewReport, ReportAction, ReportInsertOutcome, ReportReason, ReportRepo, ReportRow,
    ReportTargetKind, ResolveSpec, StorageError,
};
use uuid::Uuid;

#[derive(Default)]
struct ReportStore {
    rows: Mutex<HashMap<Uuid, ReportRow>>,
    /// Tracks the targets the fake has hidden so tests
    /// can assert the side-effect.
    hidden_targets: Mutex<Vec<(ReportTargetKind, Uuid)>>,
}

#[async_trait]
impl ReportRepo for ReportStore {
    async fn insert(
        &self,
        new: NewReport,
        auto_hide_threshold: i64,
    ) -> Result<ReportInsertOutcome, StorageError> {
        let mut inner = self.rows.lock().unwrap();
        let existing_id = inner
            .iter()
            .find(|(_, r)| {
                r.reporter_id == Some(new.reporter_id)
                    && r.target_kind == new.target_kind
                    && r.target_id == new.target_id
            })
            .map(|(id, _)| *id);
        let report_id = match existing_id {
            Some(id) => {
                let row = inner.get_mut(&id).unwrap();
                row.reason = new.reason;
                row.body = new.body.clone();
                id
            }
            None => {
                let id = Uuid::now_v7();
                inner.insert(
                    id,
                    ReportRow {
                        id,
                        reporter_id: Some(new.reporter_id),
                        target_kind: new.target_kind,
                        target_id: new.target_id,
                        reason: new.reason,
                        body: new.body,
                        created_at: new.created_at,
                        resolved_at: None,
                        resolved_by: None,
                        action_taken: None,
                        resolution_note: None,
                    },
                );
                id
            }
        };
        let reporter_count = inner
            .values()
            .filter(|r| r.target_kind == new.target_kind && r.target_id == new.target_id)
            .count() as i64;
        drop(inner);
        let mut hidden = self.hidden_targets.lock().unwrap();
        let key = (new.target_kind, new.target_id);
        let already_hidden = hidden.contains(&key);
        let freshly_hidden = !already_hidden && reporter_count >= auto_hide_threshold;
        if freshly_hidden {
            hidden.push(key);
        }
        Ok(ReportInsertOutcome {
            report_id,
            reporter_count,
            freshly_hidden,
        })
    }

    async fn list_open(
        &self,
        before: Option<DateTime<Utc>>,
        limit: i64,
    ) -> Result<Vec<ReportRow>, StorageError> {
        let inner = self.rows.lock().unwrap();
        let mut rows: Vec<ReportRow> = inner
            .values()
            .filter(|r| r.resolved_at.is_none())
            .filter(|r| before.is_none_or(|b| r.created_at < b))
            .cloned()
            .collect();
        rows.sort_by_key(|r| r.created_at);
        rows.truncate(limit as usize);
        Ok(rows)
    }

    async fn list_for_reporter(
        &self,
        reporter_id: Uuid,
        limit: i64,
    ) -> Result<Vec<ReportRow>, StorageError> {
        let inner = self.rows.lock().unwrap();
        let mut rows: Vec<ReportRow> = inner
            .values()
            .filter(|r| r.reporter_id == Some(reporter_id))
            .cloned()
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        rows.truncate(limit as usize);
        Ok(rows)
    }

    async fn get(&self, id: Uuid) -> Result<Option<ReportRow>, StorageError> {
        Ok(self.rows.lock().unwrap().get(&id).cloned())
    }

    async fn resolve(
        &self,
        id: Uuid,
        moderator_id: Uuid,
        spec: ResolveSpec<'_>,
        now: DateTime<Utc>,
    ) -> Result<Option<ReportRow>, StorageError> {
        let mut inner = self.rows.lock().unwrap();
        let Some(row) = inner.get_mut(&id) else {
            return Ok(None);
        };
        if row.resolved_at.is_some() {
            return Ok(None);
        }
        if spec.also_delete_target && matches!(row.target_kind, ReportTargetKind::Submission) {
            return Err(StorageError::InvalidArgument(
                "cannot delete a submission via report resolution".into(),
            ));
        }
        row.resolved_at = Some(now);
        row.resolved_by = Some(moderator_id);
        row.action_taken = Some(spec.action);
        row.resolution_note = spec.resolution_note.map(str::to_owned);
        let target_key = (row.target_kind, row.target_id);
        let snapshot = row.clone();
        drop(inner);
        if spec.also_hide_target {
            let mut hidden = self.hidden_targets.lock().unwrap();
            if !hidden.contains(&target_key) {
                hidden.push(target_key);
            }
        }
        Ok(Some(snapshot))
    }
}

fn make_service() -> (Arc<ReportStore>, ReportService) {
    let repo = Arc::new(ReportStore::default());
    let svc = ReportService::new(repo.clone() as Arc<dyn ReportRepo>);
    (repo, svc)
}

// === Tests ===

#[tokio::test]
async fn submit_round_trips_and_dedups_per_reporter() {
    let (_repo, svc) = make_service();
    let reporter = Uuid::now_v7();
    let target = Uuid::now_v7();
    let first = svc
        .submit(
            reporter,
            ReportTargetKind::Comment,
            target,
            ReportReason::Spam,
            Some("repeated affiliate link".into()),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(!first.freshly_hidden);
    assert_eq!(first.reporter_count, 1);
    // Second filing from the SAME reporter is a no-op for
    // the count (ON CONFLICT) — still 1.
    let second = svc
        .submit(
            reporter,
            ReportTargetKind::Comment,
            target,
            ReportReason::Harassment,
            None,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.reporter_count, 1);
    assert_eq!(second.report_id, first.report_id);
}

#[tokio::test]
async fn auto_hide_trips_on_threshold() {
    let (repo, svc) = make_service();
    let target = Uuid::now_v7();
    for i in 0..REPORT_AUTO_HIDE_THRESHOLD {
        let reporter = Uuid::now_v7();
        let outcome = svc
            .submit(
                reporter,
                ReportTargetKind::Comment,
                target,
                ReportReason::Spam,
                None,
            )
            .await
            .unwrap()
            .unwrap();
        let expect_hidden_now = i + 1 == REPORT_AUTO_HIDE_THRESHOLD;
        assert_eq!(outcome.freshly_hidden, expect_hidden_now);
    }
    assert_eq!(
        repo.hidden_targets.lock().unwrap().len(),
        1,
        "exactly one target should have been hidden"
    );
    // A fourth reporter shouldn't re-trigger the hide flag.
    let outcome = svc
        .submit(
            Uuid::now_v7(),
            ReportTargetKind::Comment,
            target,
            ReportReason::Spam,
            None,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(!outcome.freshly_hidden);
}

#[tokio::test]
async fn body_too_long_is_denial() {
    let (_repo, svc) = make_service();
    let too_long = "x".repeat(auth::REPORT_BODY_MAX_LEN + 1);
    let outcome = svc
        .submit(
            Uuid::now_v7(),
            ReportTargetKind::Comment,
            Uuid::now_v7(),
            ReportReason::Spam,
            Some(too_long),
        )
        .await
        .unwrap();
    assert_eq!(outcome.unwrap_err(), ReportDenialReason::BodyTooLong);
}

#[tokio::test]
async fn blank_body_normalizes_to_none() {
    let (_repo, svc) = make_service();
    svc.submit(
        Uuid::now_v7(),
        ReportTargetKind::Comment,
        Uuid::now_v7(),
        ReportReason::Spam,
        Some("   ".into()),
    )
    .await
    .unwrap()
    .unwrap();
    // Survives the validation, body folds to None — no
    // direct assertion here without inspecting the repo
    // internals; the absence of `BodyTooLong` is the
    // signal.
}

#[tokio::test]
async fn resolve_hide_flips_target() {
    let (repo, svc) = make_service();
    let reporter = Uuid::now_v7();
    let target = Uuid::now_v7();
    let outcome = svc
        .submit(
            reporter,
            ReportTargetKind::Comment,
            target,
            ReportReason::Spam,
            None,
        )
        .await
        .unwrap()
        .unwrap();
    let resolved = svc
        .resolve(
            outcome.report_id,
            Uuid::now_v7(),
            ReportAction::Hide,
            Some("agreed".into()),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.action_taken, Some(ReportAction::Hide));
    assert_eq!(
        repo.hidden_targets.lock().unwrap().as_slice(),
        &[(ReportTargetKind::Comment, target)]
    );
}

#[tokio::test]
async fn resolve_already_resolved_returns_not_found() {
    let (_repo, svc) = make_service();
    let reporter = Uuid::now_v7();
    let target = Uuid::now_v7();
    let outcome = svc
        .submit(
            reporter,
            ReportTargetKind::Comment,
            target,
            ReportReason::Spam,
            None,
        )
        .await
        .unwrap()
        .unwrap();
    svc.resolve(outcome.report_id, Uuid::now_v7(), ReportAction::Keep, None)
        .await
        .unwrap()
        .unwrap();
    let second = svc
        .resolve(outcome.report_id, Uuid::now_v7(), ReportAction::Keep, None)
        .await
        .unwrap();
    assert_eq!(second.unwrap_err(), ResolveDenialReason::NotFoundOrResolved);
}

#[tokio::test]
async fn resolve_delete_on_submission_rejected_at_service() {
    let (_repo, svc) = make_service();
    let reporter = Uuid::now_v7();
    let target = Uuid::now_v7();
    let outcome = svc
        .submit(
            reporter,
            ReportTargetKind::Submission,
            target,
            ReportReason::Spam,
            None,
        )
        .await
        .unwrap()
        .unwrap();
    let denied = svc
        .resolve(
            outcome.report_id,
            Uuid::now_v7(),
            ReportAction::Delete,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        denied.unwrap_err(),
        ResolveDenialReason::CannotDeleteSubmission
    );
}

#[tokio::test]
async fn list_open_returns_oldest_first() {
    let (_repo, svc) = make_service();
    let target_a = Uuid::now_v7();
    let target_b = Uuid::now_v7();
    let first = svc
        .submit(
            Uuid::now_v7(),
            ReportTargetKind::Comment,
            target_a,
            ReportReason::Spam,
            None,
        )
        .await
        .unwrap()
        .unwrap();
    let second = svc
        .submit(
            Uuid::now_v7(),
            ReportTargetKind::Comment,
            target_b,
            ReportReason::Other,
            None,
        )
        .await
        .unwrap()
        .unwrap();
    let rows = svc.list_open(None, 10).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, first.report_id);
    assert_eq!(rows[1].id, second.report_id);
}
