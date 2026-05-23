//! `submissions` repository (#5a.1).
//!
//! Community-contributed entries the moderation queue (#5a.2)
//! later processes. The repo is intentionally thin — payload
//! validation lives in the service layer (`auth::submission`)
//! so this module only worries about row I/O.
//!
//! Mirrors the `api_key_repo` split: trait-on-callee for
//! testability via in-memory fakes, plain data types for
//! callers, and `Storage`-backed sqlx implementation.
//!
//! The moderator-side mutations (approve / reject) land with
//! #5a.2; this PR ships only the author-side surface:
//!
//!   * [`SubmissionRepo::insert`] — author creates a row in
//!     `status='pending'`.
//!   * [`SubmissionRepo::list_for_user`] — author's "my
//!     submissions" page.
//!   * [`SubmissionRepo::get_for_user`] — single row,
//!     ownership-scoped so reading someone else's draft 404s.
//!   * [`SubmissionRepo::withdraw`] — author flips
//!     `status='withdrawn'` on a still-pending row.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::Row;
use sqlx::postgres::PgRow;
use uuid::Uuid;

use crate::{Storage, StorageError};

/// All four submission kinds the MVP form supports. Wire form
/// matches the `submission_kind` CHECK in migration 0013 —
/// adding a new variant requires extending both ends in
/// lockstep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionKind {
    Dataset,
    Tool,
    Connector,
    Playground,
}

impl SubmissionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dataset => "dataset",
            Self::Tool => "tool",
            Self::Connector => "connector",
            Self::Playground => "playground",
        }
    }

    /// Parse the wire string. Returns `None` for unknown kinds
    /// so the caller surfaces the right HTTP boundary (400 vs
    /// 500). The DB CHECK constraint mirrors this set so a row
    /// the table accepted will always round-trip back through
    /// `from_wire`. Named `from_wire` rather than `from_str` to
    /// dodge clippy's `should_implement_trait` lint — adding a
    /// real `FromStr` impl would mean defining an `Err` type for
    /// what callers already model as `Option`.
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "dataset" => Some(Self::Dataset),
            "tool" => Some(Self::Tool),
            "connector" => Some(Self::Connector),
            "playground" => Some(Self::Playground),
            _ => None,
        }
    }
}

/// Lifecycle state of a submission row. Mirrors the
/// `submissions_status_known` CHECK constraint in migration
/// 0013. The moderator-side transitions
/// (`pending → approved` / `pending → rejected`) ship with
/// #5a.2; this PR exercises only `pending` and
/// `pending → withdrawn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionStatus {
    Pending,
    Approved,
    Rejected,
    Withdrawn,
}

impl SubmissionStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Withdrawn => "withdrawn",
        }
    }

    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            "withdrawn" => Some(Self::Withdrawn),
            _ => None,
        }
    }
}

/// Input to [`SubmissionRepo::insert`]. The service layer is the
/// sole writer: it has already validated the per-kind payload
/// shape, derived `title` from the typed fields, and captured a
/// single `created_at` snapshot.
#[derive(Debug, Clone)]
pub struct NewSubmission {
    pub user_id: Uuid,
    pub kind: SubmissionKind,
    /// Short summary extracted from the payload at write time.
    /// Stored in its own column so the moderation queue can list
    /// without parsing the JSONB.
    pub title: String,
    /// Per-kind typed payload, already serialised to JSON by the
    /// service layer. The submission service is the only writer
    /// so the JSONB always carries a `{"kind": "...", ...}`
    /// top-level discriminator matching `kind`.
    pub payload: Value,
    /// Wall-clock `now` the service captured at the call site.
    /// Bound to `created_at` so the audit timeline shares its
    /// clock source with every subsequent `updated_at` write.
    /// The table's `DEFAULT now()` stays as a safety net for
    /// direct SQL writers (manual psql, backfills) that skip
    /// this column.
    pub created_at: DateTime<Utc>,
}

/// Row shape returned to callers. The moderator-decision
/// columns are `Option` even though the table CHECK guarantees
/// they're populated together — the option keeps the read path
/// honest if a future migration relaxes the constraint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub kind: SubmissionKind,
    pub status: SubmissionStatus,
    pub title: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub reviewed_by: Option<Uuid>,
    pub review_reason: Option<String>,
}

impl SubmissionRow {
    fn from_row(row: &PgRow) -> Result<Self, StorageError> {
        let kind_str: String = row.try_get("submission_kind").map_err(StorageError::from)?;
        let kind = SubmissionKind::from_wire(&kind_str).ok_or_else(|| {
            StorageError::Decode(format!(
                "unknown submission_kind {kind_str:?} (CHECK constraint drift?)"
            ))
        })?;
        let status_str: String = row.try_get("status").map_err(StorageError::from)?;
        let status = SubmissionStatus::from_wire(&status_str).ok_or_else(|| {
            StorageError::Decode(format!(
                "unknown submission status {status_str:?} (CHECK constraint drift?)"
            ))
        })?;
        Ok(Self {
            id: row.try_get("id").map_err(StorageError::from)?,
            user_id: row.try_get("user_id").map_err(StorageError::from)?,
            kind,
            status,
            title: row.try_get("title").map_err(StorageError::from)?,
            payload: row.try_get("payload").map_err(StorageError::from)?,
            created_at: row.try_get("created_at").map_err(StorageError::from)?,
            updated_at: row.try_get("updated_at").map_err(StorageError::from)?,
            reviewed_at: row.try_get("reviewed_at").map_err(StorageError::from)?,
            reviewed_by: row.try_get("reviewed_by").map_err(StorageError::from)?,
            review_reason: row.try_get("review_reason").map_err(StorageError::from)?,
        })
    }
}

#[async_trait]
pub trait SubmissionRepo: Send + Sync {
    /// Persist a freshly-validated submission. Always lands in
    /// `status='pending'` — the moderator-side transitions
    /// ship with #5a.2.
    async fn insert(&self, new: NewSubmission) -> Result<Uuid, StorageError>;

    /// List every submission authored by `user_id`, newest
    /// first. Includes rows in every status so the "my
    /// submissions" page can show the full history (pending /
    /// approved / rejected / withdrawn together).
    async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<SubmissionRow>, StorageError>;

    /// Fetch a single submission, scoped to the author so
    /// reading someone else's draft / pending row returns
    /// `Ok(None)` (the gateway folds it into a 404).
    async fn get_for_user(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<SubmissionRow>, StorageError>;

    /// Author-side withdraw. Idempotent: flipping an already-
    /// withdrawn row returns `Ok(None)` so the gateway can
    /// fold "not yours", "not found", and "already withdrawn"
    /// into the same response (matches the api-key revoke
    /// pattern). Only `pending` rows can be withdrawn — a
    /// moderator-approved / rejected row stays terminal.
    async fn withdraw(
        &self,
        id: Uuid,
        user_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<Option<SubmissionRow>, StorageError>;

    /// Moderator-side list (#5a.2). Returns every `pending`
    /// submission oldest-first (FIFO — the row that has been
    /// waiting longest surfaces at the top of the queue),
    /// optionally filtered to a single kind. The
    /// `submissions_pending_idx` partial index serves the
    /// unfiltered case; the `submissions_kind_status_idx`
    /// composite serves the kind-filtered case without an
    /// extra sort.
    async fn list_pending(
        &self,
        kind_filter: Option<SubmissionKind>,
    ) -> Result<Vec<SubmissionRow>, StorageError>;

    /// Moderator-side fetch by id (no user scoping). Returns
    /// the row regardless of status so the dashboard can also
    /// open an already-approved / rejected row for audit.
    async fn get_for_moderation(&self, id: Uuid) -> Result<Option<SubmissionRow>, StorageError>;

    /// Moderator-side approve, written together with the
    /// audit-log row in a single transaction so a partial
    /// commit can't leave an approved submission with no
    /// audit trail (or vice versa).
    ///
    /// `audit_metadata` is the JSONB payload the audit row
    /// will carry. The service layer composes it (kind +
    /// reason); the storage layer treats it as opaque.
    ///
    /// Returns `Ok(None)` when the id doesn't exist OR the
    /// row isn't pending anymore — the gateway folds both
    /// into a 409 response so a moderator race ("two curators
    /// clicked approve simultaneously") doesn't double-
    /// process the row. The audit log is NOT written in that
    /// case: nothing happened to audit.
    async fn approve_with_audit(
        &self,
        id: Uuid,
        moderator_id: Uuid,
        reason: Option<&str>,
        audit_metadata: &serde_json::Value,
        now: DateTime<Utc>,
    ) -> Result<Option<(SubmissionRow, Uuid)>, StorageError>;

    /// Moderator-side reject + audit log in one transaction.
    /// Same semantics as [`Self::approve_with_audit`]. The
    /// service layer enforces a non-empty reason on reject;
    /// the column stays nullable so an `approved` row without
    /// a note is representable.
    async fn reject_with_audit(
        &self,
        id: Uuid,
        moderator_id: Uuid,
        reason: &str,
        audit_metadata: &serde_json::Value,
        now: DateTime<Utc>,
    ) -> Result<Option<(SubmissionRow, Uuid)>, StorageError>;
}

#[async_trait]
impl SubmissionRepo for Storage {
    async fn insert(&self, new: NewSubmission) -> Result<Uuid, StorageError> {
        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO submissions
                (user_id, submission_kind, status, title, payload, created_at, updated_at)
             VALUES ($1, $2, 'pending', $3, $4, $5, $5)
             RETURNING id",
        )
        .bind(new.user_id)
        .bind(new.kind.as_str())
        .bind(&new.title)
        .bind(&new.payload)
        .bind(new.created_at)
        .fetch_one(self.pool())
        .await?;
        Ok(row.0)
    }

    async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<SubmissionRow>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, user_id, submission_kind, status, title, payload,
                    created_at, updated_at,
                    reviewed_at, reviewed_by, review_reason
               FROM submissions
              WHERE user_id = $1
              ORDER BY created_at DESC",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(SubmissionRow::from_row).collect()
    }

    async fn get_for_user(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<SubmissionRow>, StorageError> {
        let maybe = sqlx::query(
            "SELECT id, user_id, submission_kind, status, title, payload,
                    created_at, updated_at,
                    reviewed_at, reviewed_by, review_reason
               FROM submissions
              WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(self.pool())
        .await?;
        maybe.as_ref().map(SubmissionRow::from_row).transpose()
    }

    async fn withdraw(
        &self,
        id: Uuid,
        user_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<Option<SubmissionRow>, StorageError> {
        // The predicate scopes by author AND requires the row
        // be still in `pending` — a row already moved to
        // `approved` / `rejected` / `withdrawn` is left alone
        // and the UPDATE returns no rows. `updated_at` is
        // clamped via `GREATEST` to stay monotonic under
        // multi-instance clock skew, mirroring the session +
        // api-key repo patterns. Migration 0013 intentionally
        // does NOT install a `BEFORE UPDATE` trigger on this
        // table — the trigger function shared by the `users`
        // table would overwrite `NEW.updated_at` with `now()`
        // and silently kill this clamp.
        let maybe = sqlx::query(
            "UPDATE submissions
                SET status = 'withdrawn',
                    updated_at = GREATEST(updated_at, $3)
              WHERE id = $1
                AND user_id = $2
                AND status = 'pending'
             RETURNING id, user_id, submission_kind, status, title, payload,
                       created_at, updated_at,
                       reviewed_at, reviewed_by, review_reason",
        )
        .bind(id)
        .bind(user_id)
        .bind(now)
        .fetch_optional(self.pool())
        .await?;
        maybe.as_ref().map(SubmissionRow::from_row).transpose()
    }

    async fn list_pending(
        &self,
        kind_filter: Option<SubmissionKind>,
    ) -> Result<Vec<SubmissionRow>, StorageError> {
        // Two queries rather than a `WHERE submission_kind = $1
        // OR $1 IS NULL` so the planner can pick the most-
        // specific partial index for the unfiltered case
        // (`submissions_pending_idx`) and the composite for
        // the filtered case. Hand-merging the kind filter
        // keeps both planner paths sharp.
        let rows = if let Some(kind) = kind_filter {
            sqlx::query(
                "SELECT id, user_id, submission_kind, status, title, payload,
                        created_at, updated_at,
                        reviewed_at, reviewed_by, review_reason
                   FROM submissions
                  WHERE status = 'pending' AND submission_kind = $1
                  ORDER BY created_at ASC",
            )
            .bind(kind.as_str())
            .fetch_all(self.pool())
            .await?
        } else {
            sqlx::query(
                "SELECT id, user_id, submission_kind, status, title, payload,
                        created_at, updated_at,
                        reviewed_at, reviewed_by, review_reason
                   FROM submissions
                  WHERE status = 'pending'
                  ORDER BY created_at ASC",
            )
            .fetch_all(self.pool())
            .await?
        };
        rows.iter().map(SubmissionRow::from_row).collect()
    }

    async fn get_for_moderation(&self, id: Uuid) -> Result<Option<SubmissionRow>, StorageError> {
        let maybe = sqlx::query(
            "SELECT id, user_id, submission_kind, status, title, payload,
                    created_at, updated_at,
                    reviewed_at, reviewed_by, review_reason
               FROM submissions
              WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        maybe.as_ref().map(SubmissionRow::from_row).transpose()
    }

    async fn approve_with_audit(
        &self,
        id: Uuid,
        moderator_id: Uuid,
        reason: Option<&str>,
        audit_metadata: &serde_json::Value,
        now: DateTime<Utc>,
    ) -> Result<Option<(SubmissionRow, Uuid)>, StorageError> {
        decide_with_audit(
            self,
            DecideInputs {
                id,
                moderator_id,
                reason,
                audit_metadata,
                now,
                next_status: "approved",
                audit_action: "submission.approve",
            },
        )
        .await
    }

    async fn reject_with_audit(
        &self,
        id: Uuid,
        moderator_id: Uuid,
        reason: &str,
        audit_metadata: &serde_json::Value,
        now: DateTime<Utc>,
    ) -> Result<Option<(SubmissionRow, Uuid)>, StorageError> {
        decide_with_audit(
            self,
            DecideInputs {
                id,
                moderator_id,
                reason: Some(reason),
                audit_metadata,
                now,
                next_status: "rejected",
                audit_action: "submission.reject",
            },
        )
        .await
    }
}

/// Bundled inputs to [`decide_with_audit`]. Grouping the
/// half-dozen call-site parameters into a struct sidesteps
/// clippy's `too_many_arguments` lint and makes the call
/// sites more readable (every field is named at the use site).
struct DecideInputs<'a> {
    id: Uuid,
    moderator_id: Uuid,
    reason: Option<&'a str>,
    audit_metadata: &'a serde_json::Value,
    now: DateTime<Utc>,
    /// Either `"approved"` or `"rejected"` — pinned by the
    /// trait methods above. The `submissions_status_known`
    /// CHECK constraint rejects any typo at the DB.
    next_status: &'static str,
    /// Either `"submission.approve"` or `"submission.reject"`
    /// — pinned likewise. The `audit_logs_action_known`
    /// CHECK constraint guards the wire.
    audit_action: &'static str,
}

/// Shared implementation for `approve_with_audit` +
/// `reject_with_audit`. Begins a sqlx transaction, runs the
/// guarded `UPDATE` against `submissions`, and — only if a row
/// flipped — inserts the matching `audit_logs` row. If either
/// step fails, the transaction rolls back so the database
/// stays consistent.
async fn decide_with_audit(
    storage: &Storage,
    inputs: DecideInputs<'_>,
) -> Result<Option<(SubmissionRow, Uuid)>, StorageError> {
    let mut tx = storage.pool().begin().await?;
    let maybe = sqlx::query(
        "UPDATE submissions
            SET status = $5,
                reviewed_at = $3,
                reviewed_by = $2,
                review_reason = $4,
                updated_at = GREATEST(updated_at, $3)
          WHERE id = $1
            AND status = 'pending'
         RETURNING id, user_id, submission_kind, status, title, payload,
                   created_at, updated_at,
                   reviewed_at, reviewed_by, review_reason",
    )
    .bind(inputs.id)
    .bind(inputs.moderator_id)
    .bind(inputs.now)
    .bind(inputs.reason)
    .bind(inputs.next_status)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = maybe else {
        // No row flipped — nothing happened, nothing to audit.
        // The transaction has no writes so rollback is a no-op.
        tx.rollback().await?;
        return Ok(None);
    };
    let submission_row = SubmissionRow::from_row(&row)?;
    let (audit_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO audit_logs
            (actor_id, action, target_kind, target_id, metadata, created_at)
         VALUES ($1, $2, 'submission', $3, $4, $5)
         RETURNING id",
    )
    .bind(inputs.moderator_id)
    .bind(inputs.audit_action)
    .bind(inputs.id)
    .bind(inputs.audit_metadata)
    .bind(inputs.now)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some((submission_row, audit_id)))
}
