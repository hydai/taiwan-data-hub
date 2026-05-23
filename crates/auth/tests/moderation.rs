//! Integration tests for [`auth::ModerationService`] (#5a.2).
//!
//! Uses in-memory fakes for [`storage::SubmissionRepo`],
//! [`storage::UserRepo`], and [`storage::AuditLogRepo`] that
//! mirror the production SQL semantics: role check is the
//! first gate, approve / reject is atomic with the audit log
//! insert, and a race that flips `pending → terminal`
//! between the gate and the UPDATE collapses to
//! `NotFoundOrAlreadyDecided` (mapped to 409 by the
//! gateway).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use auth::{ModerationDenialReason, ModerationService};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use storage::{
    AuditAction, AuditLogRepo, AuditTargetKind, NewAuditLog, NewSubmission, StorageError,
    SubmissionKind, SubmissionRepo, SubmissionRow, SubmissionStatus, User, UserRepo, UserRole,
};
use uuid::Uuid;

// --- in-memory user repo ------------------------------------------------

#[derive(Default)]
struct UserStore {
    rows: Mutex<HashMap<Uuid, User>>,
}

impl UserStore {
    fn insert(&self, id: Uuid, role: UserRole) {
        let now = Utc::now();
        self.rows.lock().unwrap().insert(
            id,
            User {
                id,
                email: format!("{id}@example.test"),
                password_hash: String::new(),
                email_verified_at: Some(now),
                role,
                created_at: now,
                updated_at: now,
            },
        );
    }
}

#[async_trait]
impl UserRepo for UserStore {
    async fn insert_user(&self, _email: &str, _hash: &str) -> Result<User, StorageError> {
        unreachable!("not used by moderation tests")
    }
    async fn find_user_by_email(&self, _email: &str) -> Result<Option<User>, StorageError> {
        unreachable!("not used by moderation tests")
    }
    async fn find_user_by_id(&self, id: Uuid) -> Result<Option<User>, StorageError> {
        Ok(self.rows.lock().unwrap().get(&id).cloned())
    }
    async fn mark_email_verified(&self, _id: Uuid) -> Result<bool, StorageError> {
        unreachable!("not used by moderation tests")
    }
    async fn update_password_hash(&self, _id: Uuid, _hash: &str) -> Result<bool, StorageError> {
        unreachable!("not used by moderation tests")
    }
    async fn delete_user(&self, _id: Uuid) -> Result<bool, StorageError> {
        unreachable!("not used by moderation tests")
    }
}

// --- in-memory submission repo ------------------------------------------

#[derive(Default)]
struct SubmissionStore {
    rows: Mutex<HashMap<Uuid, Row>>,
}

#[derive(Clone)]
struct Row {
    id: Uuid,
    user_id: Uuid,
    kind: SubmissionKind,
    status: SubmissionStatus,
    title: String,
    payload: Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    reviewed_at: Option<DateTime<Utc>>,
    reviewed_by: Option<Uuid>,
    review_reason: Option<String>,
}

fn snapshot(row: &Row) -> SubmissionRow {
    SubmissionRow {
        id: row.id,
        user_id: row.user_id,
        kind: row.kind,
        status: row.status,
        title: row.title.clone(),
        payload: row.payload.clone(),
        created_at: row.created_at,
        updated_at: row.updated_at,
        reviewed_at: row.reviewed_at,
        reviewed_by: row.reviewed_by,
        review_reason: row.review_reason.clone(),
    }
}

#[async_trait]
impl SubmissionRepo for SubmissionStore {
    async fn insert(&self, new: NewSubmission) -> Result<Uuid, StorageError> {
        let id = Uuid::now_v7();
        self.rows.lock().unwrap().insert(
            id,
            Row {
                id,
                user_id: new.user_id,
                kind: new.kind,
                status: SubmissionStatus::Pending,
                title: new.title,
                payload: new.payload,
                created_at: new.created_at,
                updated_at: new.created_at,
                reviewed_at: None,
                reviewed_by: None,
                review_reason: None,
            },
        );
        Ok(id)
    }
    async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<SubmissionRow>, StorageError> {
        let inner = self.rows.lock().unwrap();
        Ok(inner
            .values()
            .filter(|r| r.user_id == user_id)
            .map(snapshot)
            .collect())
    }
    async fn get_for_user(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<SubmissionRow>, StorageError> {
        let inner = self.rows.lock().unwrap();
        Ok(inner
            .get(&id)
            .filter(|r| r.user_id == user_id)
            .map(snapshot))
    }
    async fn withdraw(
        &self,
        id: Uuid,
        user_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<Option<SubmissionRow>, StorageError> {
        let mut inner = self.rows.lock().unwrap();
        let Some(row) = inner.get_mut(&id) else {
            return Ok(None);
        };
        if row.user_id != user_id || row.status != SubmissionStatus::Pending {
            return Ok(None);
        }
        row.status = SubmissionStatus::Withdrawn;
        row.updated_at = row.updated_at.max(now);
        Ok(Some(snapshot(row)))
    }
    async fn list_pending(
        &self,
        kind_filter: Option<SubmissionKind>,
    ) -> Result<Vec<SubmissionRow>, StorageError> {
        let inner = self.rows.lock().unwrap();
        let mut rows: Vec<SubmissionRow> = inner
            .values()
            .filter(|r| {
                r.status == SubmissionStatus::Pending && kind_filter.is_none_or(|k| r.kind == k)
            })
            .map(snapshot)
            .collect();
        rows.sort_by_key(|r| r.created_at);
        Ok(rows)
    }
    async fn get_for_moderation(&self, id: Uuid) -> Result<Option<SubmissionRow>, StorageError> {
        Ok(self.rows.lock().unwrap().get(&id).map(snapshot))
    }
    async fn approve(
        &self,
        id: Uuid,
        moderator_id: Uuid,
        reason: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Option<SubmissionRow>, StorageError> {
        let mut inner = self.rows.lock().unwrap();
        let Some(row) = inner.get_mut(&id) else {
            return Ok(None);
        };
        if row.status != SubmissionStatus::Pending {
            return Ok(None);
        }
        row.status = SubmissionStatus::Approved;
        row.reviewed_at = Some(now);
        row.reviewed_by = Some(moderator_id);
        row.review_reason = reason.map(str::to_owned);
        row.updated_at = row.updated_at.max(now);
        Ok(Some(snapshot(row)))
    }
    async fn reject(
        &self,
        id: Uuid,
        moderator_id: Uuid,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<SubmissionRow>, StorageError> {
        let mut inner = self.rows.lock().unwrap();
        let Some(row) = inner.get_mut(&id) else {
            return Ok(None);
        };
        if row.status != SubmissionStatus::Pending {
            return Ok(None);
        }
        row.status = SubmissionStatus::Rejected;
        row.reviewed_at = Some(now);
        row.reviewed_by = Some(moderator_id);
        row.review_reason = Some(reason.to_owned());
        row.updated_at = row.updated_at.max(now);
        Ok(Some(snapshot(row)))
    }
}

// --- in-memory audit log repo -------------------------------------------

#[derive(Default)]
struct AuditStore {
    rows: Mutex<Vec<AuditRecord>>,
}

#[derive(Clone)]
struct AuditRecord {
    #[allow(dead_code)]
    id: Uuid,
    actor_id: Option<Uuid>,
    action: AuditAction,
    target_kind: AuditTargetKind,
    target_id: Option<Uuid>,
    metadata: Value,
    #[allow(dead_code)]
    created_at: DateTime<Utc>,
}

#[async_trait]
impl AuditLogRepo for AuditStore {
    async fn insert(&self, new: NewAuditLog) -> Result<Uuid, StorageError> {
        let id = Uuid::now_v7();
        self.rows.lock().unwrap().push(AuditRecord {
            id,
            actor_id: new.actor_id,
            action: new.action,
            target_kind: new.target_kind,
            target_id: new.target_id,
            metadata: new.metadata,
            created_at: new.created_at,
        });
        Ok(id)
    }
}

// --- fixtures ----------------------------------------------------------

struct Harness {
    svc: ModerationService,
    submissions: Arc<SubmissionStore>,
    audit: Arc<AuditStore>,
    users: Arc<UserStore>,
}

fn build_harness() -> Harness {
    let submissions: Arc<SubmissionStore> = Arc::new(SubmissionStore::default());
    let users: Arc<UserStore> = Arc::new(UserStore::default());
    let audit: Arc<AuditStore> = Arc::new(AuditStore::default());
    let svc = ModerationService::new(
        submissions.clone() as Arc<dyn SubmissionRepo>,
        users.clone() as Arc<dyn UserRepo>,
        audit.clone() as Arc<dyn AuditLogRepo>,
    );
    Harness {
        svc,
        submissions,
        audit,
        users,
    }
}

async fn seed_submission(
    submissions: &SubmissionStore,
    author: Uuid,
    kind: SubmissionKind,
) -> Uuid {
    submissions
        .insert(NewSubmission {
            user_id: author,
            kind,
            title: "test".into(),
            payload: json!({"kind": kind.as_str()}),
            created_at: Utc::now(),
        })
        .await
        .unwrap()
}

// --- tests -------------------------------------------------------------

#[tokio::test]
async fn require_moderator_blocks_regular_user() {
    let h = build_harness();
    let alice = Uuid::now_v7();
    h.users.insert(alice, UserRole::User);
    let err = h.svc.require_moderator(alice).await.unwrap_err();
    assert_eq!(err, ModerationDenialReason::Forbidden);
}

#[tokio::test]
async fn require_moderator_admits_curator() {
    let h = build_harness();
    let bob = Uuid::now_v7();
    h.users.insert(bob, UserRole::Curator);
    let role = h.svc.require_moderator(bob).await.unwrap();
    assert_eq!(role, UserRole::Curator);
}

#[tokio::test]
async fn approve_flips_status_and_writes_audit_log() {
    let h = build_harness();
    let author = Uuid::now_v7();
    let mod_id = Uuid::now_v7();
    h.users.insert(mod_id, UserRole::Moderator);
    let sub_id = seed_submission(&h.submissions, author, SubmissionKind::Dataset).await;
    let outcome = h
        .svc
        .approve(mod_id, sub_id, Some("looks good".into()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome.submission.status, SubmissionStatus::Approved);
    assert_eq!(outcome.submission.reviewed_by, Some(mod_id));
    assert_eq!(
        outcome.submission.review_reason.as_deref(),
        Some("looks good")
    );
    let audit = h.audit.rows.lock().unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].action, AuditAction::SubmissionApprove);
    assert_eq!(audit[0].actor_id, Some(mod_id));
    assert_eq!(audit[0].target_kind, AuditTargetKind::Submission);
    assert_eq!(audit[0].target_id, Some(sub_id));
    assert_eq!(audit[0].metadata["submission_kind"], "dataset");
}

#[tokio::test]
async fn approve_with_blank_reason_persists_null() {
    let h = build_harness();
    let author = Uuid::now_v7();
    let mod_id = Uuid::now_v7();
    h.users.insert(mod_id, UserRole::Moderator);
    let sub_id = seed_submission(&h.submissions, author, SubmissionKind::Tool).await;
    let outcome = h
        .svc
        .approve(mod_id, sub_id, Some("   ".into()))
        .await
        .unwrap()
        .unwrap();
    assert!(outcome.submission.review_reason.is_none());
}

#[tokio::test]
async fn reject_requires_non_blank_reason() {
    let h = build_harness();
    let author = Uuid::now_v7();
    let mod_id = Uuid::now_v7();
    h.users.insert(mod_id, UserRole::Moderator);
    let sub_id = seed_submission(&h.submissions, author, SubmissionKind::Connector).await;
    let denial = h
        .svc
        .reject(mod_id, sub_id, "  ".into())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(denial, ModerationDenialReason::MissingRejectReason);
    // The row stays pending because the service short-
    // circuited before the storage UPDATE.
    let row = h
        .submissions
        .get_for_moderation(sub_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, SubmissionStatus::Pending);
}

#[tokio::test]
async fn reject_flips_status_and_writes_reason() {
    let h = build_harness();
    let author = Uuid::now_v7();
    let mod_id = Uuid::now_v7();
    h.users.insert(mod_id, UserRole::Moderator);
    let sub_id = seed_submission(&h.submissions, author, SubmissionKind::Playground).await;
    let outcome = h
        .svc
        .reject(mod_id, sub_id, "url is dead".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome.submission.status, SubmissionStatus::Rejected);
    assert_eq!(
        outcome.submission.review_reason.as_deref(),
        Some("url is dead")
    );
    let audit = h.audit.rows.lock().unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].action, AuditAction::SubmissionReject);
}

#[tokio::test]
async fn approve_on_already_decided_returns_conflict_denial() {
    let h = build_harness();
    let author = Uuid::now_v7();
    let mod_id = Uuid::now_v7();
    h.users.insert(mod_id, UserRole::Moderator);
    let sub_id = seed_submission(&h.submissions, author, SubmissionKind::Dataset).await;
    let _first = h.svc.approve(mod_id, sub_id, None).await.unwrap().unwrap();
    // Second call: another moderator races us.
    let denial = h
        .svc
        .approve(mod_id, sub_id, None)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(denial, ModerationDenialReason::NotFoundOrAlreadyDecided);
    // Only the first decision audit-logged.
    assert_eq!(h.audit.rows.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn list_pending_respects_kind_filter() {
    let h = build_harness();
    let author = Uuid::now_v7();
    let mod_id = Uuid::now_v7();
    h.users.insert(mod_id, UserRole::Moderator);
    let dataset = seed_submission(&h.submissions, author, SubmissionKind::Dataset).await;
    let _tool = seed_submission(&h.submissions, author, SubmissionKind::Tool).await;
    let datasets_only = h
        .svc
        .list_pending(Some(SubmissionKind::Dataset))
        .await
        .unwrap();
    assert_eq!(datasets_only.len(), 1);
    assert_eq!(datasets_only[0].id, dataset);
    let all = h.svc.list_pending(None).await.unwrap();
    assert_eq!(all.len(), 2);
}
