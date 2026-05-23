//! `audit_logs` repository (#5a.2).
//!
//! Append-only ledger of moderator decisions. The repo
//! exposes only an `insert` because the audit table is
//! deliberately write-once at the application layer —
//! UPDATE / DELETE require a DB-side privilege escalation.
//! Listing endpoints (per-actor history, per-target history)
//! land with the moderator dashboard UI in a later PR; the
//! row store stays minimal here.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{Storage, StorageError};

/// Discriminator on `audit_logs.action`. Mirrors the
/// `audit_logs_action_known` CHECK in migration 0014.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditAction {
    SubmissionApprove,
    SubmissionReject,
    /// Logged when an approved dataset submission is
    /// successfully written to the `datasets` table.
    SubmissionPromoteDataset,
}

impl AuditAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SubmissionApprove => "submission.approve",
            Self::SubmissionReject => "submission.reject",
            Self::SubmissionPromoteDataset => "submission.promote_dataset",
        }
    }
}

/// Discriminator on `audit_logs.target_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditTargetKind {
    Submission,
    Dataset,
}

impl AuditTargetKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Submission => "submission",
            Self::Dataset => "dataset",
        }
    }
}

/// Input to [`AuditLogRepo::insert`]. The service layer
/// captures `created_at` so the moderator + audit timeline
/// share a clock source.
#[derive(Debug, Clone)]
pub struct NewAuditLog {
    pub actor_id: Option<Uuid>,
    pub action: AuditAction,
    pub target_kind: AuditTargetKind,
    pub target_id: Option<Uuid>,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
}

#[async_trait]
pub trait AuditLogRepo: Send + Sync {
    /// Append a new audit row. Returns the assigned UUID so
    /// the caller can attach it to its own response if needed.
    async fn insert(&self, new: NewAuditLog) -> Result<Uuid, StorageError>;
}

/// Single source of truth for the `audit_logs` INSERT.
/// Takes a `&mut PgConnection` so both the
/// pool-targeted [`AuditLogRepo::insert`] and the transactional
/// caller in `submission_repo::decide_with_audit` can reuse
/// the same SQL — a schema change here automatically
/// propagates to both call sites.
///
/// `crate`-visible so callers in sibling modules can opt in;
/// not part of the public API.
pub(crate) async fn insert_audit_log_inner(
    conn: &mut PgConnection,
    new: &NewAuditLog,
) -> Result<Uuid, StorageError> {
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO audit_logs
            (actor_id, action, target_kind, target_id, metadata, created_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING id",
    )
    .bind(new.actor_id)
    .bind(new.action.as_str())
    .bind(new.target_kind.as_str())
    .bind(new.target_id)
    .bind(&new.metadata)
    .bind(new.created_at)
    .fetch_one(conn)
    .await?;
    Ok(id)
}

#[async_trait]
impl AuditLogRepo for Storage {
    async fn insert(&self, new: NewAuditLog) -> Result<Uuid, StorageError> {
        let mut conn = self.pool().acquire().await?;
        insert_audit_log_inner(&mut conn, &new).await
    }
}
