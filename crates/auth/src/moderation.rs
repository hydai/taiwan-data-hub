//! Moderation service (#5a.2).
//!
//! Wraps the moderator-side operations on the
//! `submissions` table behind a role-checking facade, and
//! writes an audit log entry alongside every decision so the
//! "who approved this" question always has an answer.
//!
//! The service does NOT promote approved submissions into the
//! canonical `datasets` / `tools` / `connectors` tables — the
//! moderator dashboard ships only the approve / reject /
//! audit-log surface in #5a.2. Dataset promotion lands in
//! #5b.6 alongside the provenance metadata work, where the
//! ETL framework already exists to derive the i18n shape +
//! domain reconciliation needed.

use std::sync::Arc;

use chrono::Utc;
use serde_json::json;
use storage::{
    AuditAction, AuditLogRepo, AuditTargetKind, NewAuditLog, SubmissionRepo, SubmissionRow,
    UserRepo, UserRole,
};
use uuid::Uuid;

use crate::error::AuthError;

/// Outcome of an approve / reject call. Carries the row
/// post-decision so the gateway can echo it back to the
/// moderation UI without a second round trip.
#[derive(Debug, Clone)]
pub struct Decision {
    pub submission: SubmissionRow,
    pub audit_log_id: Uuid,
}

/// Why a moderation call rejected the caller's request.
///
/// Distinct variants so the gateway can choose the right HTTP
/// status — 403 on a permission gap, 404 on a missing row,
/// 409 on a race / already-terminal row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModerationDenialReason {
    /// Caller's role is below the moderation tier.
    Forbidden,
    /// The submission id doesn't exist OR the caller is acting
    /// on a row that the row store rejected (e.g. the row
    /// was already approved / rejected by another moderator
    /// between this client's list-load and decision POST).
    NotFoundOrAlreadyDecided,
    /// The caller passed a reject without the required reason
    /// (or a reason that was blank after trim).
    MissingRejectReason,
}

#[derive(Clone)]
pub struct ModerationService {
    submissions: Arc<dyn SubmissionRepo>,
    users: Arc<dyn UserRepo>,
    audit: Arc<dyn AuditLogRepo>,
}

impl std::fmt::Debug for ModerationService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModerationService").finish_non_exhaustive()
    }
}

impl ModerationService {
    #[must_use]
    pub fn new(
        submissions: Arc<dyn SubmissionRepo>,
        users: Arc<dyn UserRepo>,
        audit: Arc<dyn AuditLogRepo>,
    ) -> Self {
        Self {
            submissions,
            users,
            audit,
        }
    }

    /// Look up the caller's role + check the moderator gate.
    /// Returns `Ok(())` if the user is at least a moderator,
    /// `Err(Forbidden)` otherwise. Used by every moderation
    /// endpoint as the first gate.
    pub async fn require_moderator(
        &self,
        user_id: Uuid,
    ) -> Result<UserRole, ModerationDenialReason> {
        let user = self
            .users
            .find_user_by_id(user_id)
            .await
            .map_err(|_| ModerationDenialReason::Forbidden)?
            .ok_or(ModerationDenialReason::Forbidden)?;
        if !user.role.can_moderate() {
            return Err(ModerationDenialReason::Forbidden);
        }
        Ok(user.role)
    }

    /// List pending submissions for the moderation queue.
    /// Caller must have already cleared the role gate via
    /// [`Self::require_moderator`].
    pub async fn list_pending(
        &self,
        kind_filter: Option<storage::SubmissionKind>,
    ) -> Result<Vec<SubmissionRow>, AuthError> {
        Ok(self.submissions.list_pending(kind_filter).await?)
    }

    /// Moderator detail view of a single submission, regardless
    /// of status (so an already-decided row can still be opened
    /// for audit).
    pub async fn get(&self, id: Uuid) -> Result<Option<SubmissionRow>, AuthError> {
        Ok(self.submissions.get_for_moderation(id).await?)
    }

    /// Approve a pending submission. Writes an audit log row in
    /// the same call so the timestamp + actor are consistent.
    /// `reason` is optional on approve — moderators may leave a
    /// note for the author but aren't required to.
    pub async fn approve(
        &self,
        moderator_id: Uuid,
        submission_id: Uuid,
        reason: Option<String>,
    ) -> Result<Result<Decision, ModerationDenialReason>, AuthError> {
        let now = Utc::now();
        let trimmed = reason
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let Some(row) = self
            .submissions
            .approve(submission_id, moderator_id, trimmed.as_deref(), now)
            .await?
        else {
            return Ok(Err(ModerationDenialReason::NotFoundOrAlreadyDecided));
        };
        let audit_log_id = self
            .audit
            .insert(NewAuditLog {
                actor_id: Some(moderator_id),
                action: AuditAction::SubmissionApprove,
                target_kind: AuditTargetKind::Submission,
                target_id: Some(submission_id),
                metadata: json!({
                    "submission_kind": row.kind.as_str(),
                    "reason": trimmed,
                }),
                created_at: now,
            })
            .await?;
        Ok(Ok(Decision {
            submission: row,
            audit_log_id,
        }))
    }

    /// Reject a pending submission. The reason is mandatory on
    /// reject; an empty trim folds to [`ModerationDenialReason::MissingRejectReason`].
    pub async fn reject(
        &self,
        moderator_id: Uuid,
        submission_id: Uuid,
        reason: String,
    ) -> Result<Result<Decision, ModerationDenialReason>, AuthError> {
        let trimmed = reason.trim().to_owned();
        if trimmed.is_empty() {
            return Ok(Err(ModerationDenialReason::MissingRejectReason));
        }
        let now = Utc::now();
        let Some(row) = self
            .submissions
            .reject(submission_id, moderator_id, &trimmed, now)
            .await?
        else {
            return Ok(Err(ModerationDenialReason::NotFoundOrAlreadyDecided));
        };
        let audit_log_id = self
            .audit
            .insert(NewAuditLog {
                actor_id: Some(moderator_id),
                action: AuditAction::SubmissionReject,
                target_kind: AuditTargetKind::Submission,
                target_id: Some(submission_id),
                metadata: json!({
                    "submission_kind": row.kind.as_str(),
                    "reason": trimmed,
                }),
                created_at: now,
            })
            .await?;
        Ok(Ok(Decision {
            submission: row,
            audit_log_id,
        }))
    }
}
