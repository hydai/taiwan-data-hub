//! Content reports + moderator dispositioning (#5a.6).
//!
//! Wraps [`storage::ReportRepo`] with:
//!
//!   * `submit` — validates the optional `body` length,
//!     normalises whitespace, runs an idempotent insert,
//!     and notes whether the auto-hide threshold just
//!     tripped.
//!   * `list_open` / `list_for_reporter` — moderator and
//!     reporter views respectively.
//!   * `resolve` — moderator-only; the action enum maps
//!     directly to repo-level side effects on the target.
//!
//! Moderator role gating reuses [`ModerationService::
//! require_moderator`] (issue #5a.2) — the same RBAC
//! tree applies to submission moderation and report
//! dispositioning.

use std::sync::Arc;

use chrono::Utc;
use storage::{
    NewReport, ReportAction, ReportInsertOutcome, ReportReason, ReportRepo, ReportRow,
    ReportTargetKind, ResolveSpec,
};
use uuid::Uuid;

use crate::error::AuthError;

/// Number of distinct reporters required before a target
/// flips to `hidden_at`. Lives in service code so the
/// threshold can move without a schema migration.
pub const REPORT_AUTO_HIDE_THRESHOLD: i64 = 3;

/// Cap on the optional report body. Lines up with the
/// submission description cap so the UI can reuse the
/// same textarea component.
pub const REPORT_BODY_MAX_LEN: usize = 2048;

/// Why a report write was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportDenialReason {
    /// Optional body exceeded [`REPORT_BODY_MAX_LEN`].
    BodyTooLong,
}

/// Why a moderator-side resolution failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveDenialReason {
    /// Report id not found, or already resolved.
    NotFoundOrResolved,
    /// `ReportAction::Delete` requested with a submission
    /// target — submissions can't be hard-deleted via the
    /// report path; the operator must use the moderation
    /// queue for that.
    CannotDeleteSubmission,
}

#[derive(Clone)]
pub struct ReportService {
    repo: Arc<dyn ReportRepo>,
}

impl std::fmt::Debug for ReportService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReportService").finish_non_exhaustive()
    }
}

impl ReportService {
    #[must_use]
    pub fn new(repo: Arc<dyn ReportRepo>) -> Self {
        Self { repo }
    }

    /// File or update a report against `(target_kind,
    /// target_id)`. Idempotent on `(reporter, target)`.
    /// A whitespace-only body normalises to `None` so the
    /// moderator queue doesn't show empty quoted blocks.
    pub async fn submit(
        &self,
        reporter_id: Uuid,
        target_kind: ReportTargetKind,
        target_id: Uuid,
        reason: ReportReason,
        body: Option<String>,
    ) -> Result<Result<ReportInsertOutcome, ReportDenialReason>, AuthError> {
        let body = body.and_then(|s| {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        });
        if let Some(b) = body.as_deref()
            && b.chars().count() > REPORT_BODY_MAX_LEN
        {
            return Ok(Err(ReportDenialReason::BodyTooLong));
        }
        let outcome = self
            .repo
            .insert(
                NewReport {
                    reporter_id,
                    target_kind,
                    target_id,
                    reason,
                    body,
                    created_at: Utc::now(),
                },
                REPORT_AUTO_HIDE_THRESHOLD,
            )
            .await?;
        Ok(Ok(outcome))
    }

    /// Moderator queue. Caller must have already passed
    /// [`crate::ModerationService::require_moderator`].
    pub async fn list_open(
        &self,
        before: Option<chrono::DateTime<Utc>>,
        limit: i64,
    ) -> Result<Vec<ReportRow>, AuthError> {
        Ok(self.repo.list_open(before, limit).await?)
    }

    /// Reporter's own filed reports (with resolution
    /// status when present).
    pub async fn list_for_reporter(
        &self,
        reporter_id: Uuid,
        limit: i64,
    ) -> Result<Vec<ReportRow>, AuthError> {
        Ok(self.repo.list_for_reporter(reporter_id, limit).await?)
    }

    /// Disposition a report. Service-layer gate on
    /// `Delete + Submission` because hard-deleting a
    /// submission would bypass the moderation lifecycle.
    /// `Hide` is allowed for both kinds and the repo
    /// flips the target's `hidden_at` column.
    pub async fn resolve(
        &self,
        id: Uuid,
        moderator_id: Uuid,
        action: ReportAction,
        resolution_note: Option<String>,
    ) -> Result<Result<ReportRow, ResolveDenialReason>, AuthError> {
        // Need the row to type-check the action against
        // the target kind without trusting the caller.
        let Some(existing) = self.repo.get(id).await? else {
            return Ok(Err(ResolveDenialReason::NotFoundOrResolved));
        };
        if existing.resolved_at.is_some() {
            return Ok(Err(ResolveDenialReason::NotFoundOrResolved));
        }
        if matches!(action, ReportAction::Delete)
            && matches!(existing.target_kind, ReportTargetKind::Submission)
        {
            return Ok(Err(ResolveDenialReason::CannotDeleteSubmission));
        }
        let resolved = self
            .repo
            .resolve(
                id,
                moderator_id,
                ResolveSpec {
                    action,
                    resolution_note: resolution_note.as_deref(),
                    also_hide_target: matches!(action, ReportAction::Hide),
                    also_delete_target: matches!(action, ReportAction::Delete),
                },
                Utc::now(),
            )
            .await?;
        match resolved {
            Some(row) => Ok(Ok(row)),
            None => Ok(Err(ResolveDenialReason::NotFoundOrResolved)),
        }
    }
}
