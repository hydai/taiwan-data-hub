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
use storage::{SubmissionRepo, SubmissionRow, UserRepo, UserRole};
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
/// status. The current `gateway::moderation_routes` mapping
/// is:
///
///   * [`Self::Forbidden`] → 403 (insufficient role or
///     unknown user — the gate deliberately folds both so a
///     probing attacker can't enumerate accounts).
///   * [`Self::NotFoundOrAlreadyDecided`] → 409 (the id
///     either never existed in a pending state, or was
///     decided by another moderator between this client's
///     list-load and decision POST — the gateway uses a
///     single status code for both because distinguishing
///     them would let an attacker probe for valid IDs by
///     watching the response code).
///   * [`Self::MissingRejectReason`] → 400 (`reject` without
///     a non-empty `reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModerationDenialReason {
    /// Caller's role is below the moderation tier, OR the
    /// caller's user id is unknown to the row store. The
    /// gate folds both so an attacker can't probe for valid
    /// accounts by status code.
    Forbidden,
    /// The submission id doesn't exist OR the caller is acting
    /// on a row that the row store rejected (e.g. the row
    /// was already approved / rejected by another moderator
    /// between this client's list-load and decision POST).
    /// The gateway maps both into 409 — the UI uses that to
    /// know it should refresh the queue.
    NotFoundOrAlreadyDecided,
    /// The caller passed a reject without the required reason
    /// (or a reason that was blank after trim).
    MissingRejectReason,
}

#[derive(Clone)]
pub struct ModerationService {
    submissions: Arc<dyn SubmissionRepo>,
    users: Arc<dyn UserRepo>,
}

impl std::fmt::Debug for ModerationService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModerationService").finish_non_exhaustive()
    }
}

impl ModerationService {
    #[must_use]
    pub fn new(submissions: Arc<dyn SubmissionRepo>, users: Arc<dyn UserRepo>) -> Self {
        Self { submissions, users }
    }

    /// Look up the caller's role + check the moderator gate.
    /// Returns the role on success, an outer
    /// [`AuthError::Storage`] when the DB call fails (mapped
    /// to 500 by the gateway), or an inner
    /// `Err(Forbidden)` when the user is missing or has an
    /// insufficient role.
    ///
    /// The two error layers are intentionally distinct so a
    /// transient DB outage doesn't surface as 403 — a 500
    /// signals an operator-actionable failure, while 403
    /// signals an end-user permission issue.
    ///
    /// Uses [`UserRepo::find_user_role`] (selects only the
    /// role column, served by the `users` PRIMARY KEY index)
    /// so the hot-path admin request stays cheap — single
    /// btree probe, no `password_hash` materialisation.
    pub async fn require_moderator(
        &self,
        user_id: Uuid,
    ) -> Result<Result<UserRole, ModerationDenialReason>, AuthError> {
        let Some(role) = self.users.find_user_role(user_id).await? else {
            return Ok(Err(ModerationDenialReason::Forbidden));
        };
        if !role.can_moderate() {
            return Ok(Err(ModerationDenialReason::Forbidden));
        }
        Ok(Ok(role))
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

    /// Approve a pending submission. The status flip + audit
    /// log row are written in a single DB transaction
    /// (`SubmissionRepo::approve_with_audit`) so a partial
    /// commit can't leave an approved submission with no
    /// audit trail. The audit metadata's `submission_kind`
    /// is derived inside that transaction from the post-
    /// UPDATE row — no service-side pre-read.
    /// `reason` is optional on approve — moderators may leave
    /// a note for the author but aren't required to.
    pub async fn approve(
        &self,
        moderator_id: Uuid,
        submission_id: Uuid,
        reason: Option<String>,
    ) -> Result<Result<Decision, ModerationDenialReason>, AuthError> {
        let trimmed = reason
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let outcome = self
            .submissions
            .approve_with_audit(submission_id, moderator_id, trimmed.as_deref(), Utc::now())
            .await?;
        let Some((row, audit_log_id)) = outcome else {
            return Ok(Err(ModerationDenialReason::NotFoundOrAlreadyDecided));
        };
        Ok(Ok(Decision {
            submission: row,
            audit_log_id,
        }))
    }

    /// Reject a pending submission. The reason is mandatory on
    /// reject; an empty trim folds to [`ModerationDenialReason::MissingRejectReason`].
    /// Status flip + audit log row write happen in a single
    /// DB transaction.
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
        let outcome = self
            .submissions
            .reject_with_audit(submission_id, moderator_id, &trimmed, Utc::now())
            .await?;
        let Some((row, audit_log_id)) = outcome else {
            return Ok(Err(ModerationDenialReason::NotFoundOrAlreadyDecided));
        };
        Ok(Ok(Decision {
            submission: row,
            audit_log_id,
        }))
    }
}
