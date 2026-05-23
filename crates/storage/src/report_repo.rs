//! Content reports (#5a.6) — per-(reporter, target)
//! flag rows + the moderator queue + auto-hide flips on
//! the backing comment/submission row.
//!
//! Three surfaces share this module because their writes
//! interlock: filing a fresh report under the auto-hide
//! threshold must atomically set `hidden_at` on the
//! target, so the repo runs both writes in one
//! transaction.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::postgres::PgRow;
use uuid::Uuid;

use crate::{Storage, StorageError};

/// What's being reported. Two-element set today; extend
/// the CHECK + this enum in lockstep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportTargetKind {
    Comment,
    Submission,
}

impl ReportTargetKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::Submission => "submission",
        }
    }

    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "comment" => Some(Self::Comment),
            "submission" => Some(Self::Submission),
            _ => None,
        }
    }
}

/// Coarse-grained reason categories the UI surfaces in a
/// radio group. Mirrors the SQL CHECK exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportReason {
    Spam,
    Harassment,
    OffTopic,
    Illegal,
    Inaccurate,
    Other,
}

impl ReportReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spam => "spam",
            Self::Harassment => "harassment",
            Self::OffTopic => "off_topic",
            Self::Illegal => "illegal",
            Self::Inaccurate => "inaccurate",
            Self::Other => "other",
        }
    }

    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "spam" => Some(Self::Spam),
            "harassment" => Some(Self::Harassment),
            "off_topic" => Some(Self::OffTopic),
            "illegal" => Some(Self::Illegal),
            "inaccurate" => Some(Self::Inaccurate),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

/// Action a moderator took on a report. Encoded onto
/// the wire as the `action_taken` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportAction {
    /// Hide the target (sets `hidden_at` on the row).
    Hide,
    /// Keep the target visible — no further action.
    Keep,
    /// Delete the target outright (handled by the
    /// service via the respective repo).
    Delete,
    /// Send a warning to the target's author.
    /// Recorded but the delivery mechanism is a follow-up.
    WarnAuthor,
}

impl ReportAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hide => "hide",
            Self::Keep => "keep",
            Self::Delete => "delete",
            Self::WarnAuthor => "warn_author",
        }
    }

    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "hide" => Some(Self::Hide),
            "keep" => Some(Self::Keep),
            "delete" => Some(Self::Delete),
            "warn_author" => Some(Self::WarnAuthor),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewReport {
    pub reporter_id: Uuid,
    pub target_kind: ReportTargetKind,
    pub target_id: Uuid,
    pub reason: ReportReason,
    pub body: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportRow {
    pub id: Uuid,
    pub reporter_id: Option<Uuid>,
    pub target_kind: ReportTargetKind,
    pub target_id: Uuid,
    pub reason: ReportReason,
    pub body: Option<String>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub resolved_by: Option<Uuid>,
    pub action_taken: Option<ReportAction>,
    pub resolution_note: Option<String>,
}

impl ReportRow {
    fn from_row(row: &PgRow) -> Result<Self, StorageError> {
        let kind_str: String = row.try_get("target_kind").map_err(StorageError::from)?;
        let target_kind = ReportTargetKind::from_wire(&kind_str).ok_or_else(|| {
            StorageError::Decode(format!(
                "unknown reports.target_kind {kind_str:?} (CHECK drift?)"
            ))
        })?;
        let reason_str: String = row.try_get("reason_category").map_err(StorageError::from)?;
        let reason = ReportReason::from_wire(&reason_str).ok_or_else(|| {
            StorageError::Decode(format!(
                "unknown reports.reason_category {reason_str:?} (CHECK drift?)"
            ))
        })?;
        let action_str: Option<String> = row.try_get("action_taken").map_err(StorageError::from)?;
        let action_taken = match action_str {
            None => None,
            Some(s) => Some(ReportAction::from_wire(&s).ok_or_else(|| {
                StorageError::Decode(format!("unknown reports.action_taken {s:?} (CHECK drift?)"))
            })?),
        };
        Ok(Self {
            id: row.try_get("id").map_err(StorageError::from)?,
            reporter_id: row.try_get("reporter_id").map_err(StorageError::from)?,
            target_kind,
            target_id: row.try_get("target_id").map_err(StorageError::from)?,
            reason,
            body: row.try_get("body").map_err(StorageError::from)?,
            created_at: row.try_get("created_at").map_err(StorageError::from)?,
            resolved_at: row.try_get("resolved_at").map_err(StorageError::from)?,
            resolved_by: row.try_get("resolved_by").map_err(StorageError::from)?,
            action_taken,
            resolution_note: row.try_get("resolution_note").map_err(StorageError::from)?,
        })
    }
}

/// Outcome of [`ReportRepo::insert`]. Lets the service
/// react when the auto-hide threshold was crossed by THIS
/// filing without making a second query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InsertOutcome {
    pub report_id: Uuid,
    /// `true` when a fresh row was inserted. `false`
    /// when the upsert hit an existing
    /// `(reporter, target)` row (which the gateway maps
    /// to 200 vs the freshly-created 201).
    pub created: bool,
    /// Unresolved reporters on this target *after* this
    /// insert, including the caller. Resolved reports
    /// don't count — once a moderator dispositions a
    /// report it stops contributing to the auto-hide
    /// threshold.
    pub reporter_count: i64,
    /// `true` when the target's `hidden_at` was just
    /// flipped from `NULL` → `now()` by this insert.
    pub freshly_hidden: bool,
}

/// Disposition the service hands to
/// [`ReportRepo::resolve`]. Bundles the action enum,
/// optional moderator note, and the side-effect flags
/// into one record so the trait signature stays
/// readable.
#[derive(Debug, Clone)]
pub struct ResolveSpec<'a> {
    pub action: ReportAction,
    pub resolution_note: Option<&'a str>,
    /// When true, the target's `hidden_at` is set to
    /// `now`. The service sets this in lockstep with
    /// [`ReportAction::Hide`].
    pub also_hide_target: bool,
    /// When true, the target's `hidden_at` is cleared
    /// (set to `NULL`). The service sets this on
    /// [`ReportAction::Keep`] so an auto-hidden target
    /// becomes visible again once a moderator vouches
    /// for it. The unhide is unconditional — if the
    /// target was visible to begin with, this is a
    /// no-op.
    pub also_unhide_target: bool,
    /// When true, the comment row is soft-deleted. Has
    /// no effect for submission targets — the service
    /// refuses that combination upstream.
    pub also_delete_target: bool,
}

#[async_trait]
pub trait ReportRepo: Send + Sync {
    /// File a report. Idempotent on `(reporter, target)`.
    /// If the insert crosses the threshold, the target's
    /// backing row gets `hidden_at = now` in the same
    /// transaction.
    async fn insert(
        &self,
        new: NewReport,
        auto_hide_threshold: i64,
    ) -> Result<InsertOutcome, StorageError>;

    /// List open reports (oldest first), paginated.
    async fn list_open(
        &self,
        before: Option<DateTime<Utc>>,
        limit: i64,
    ) -> Result<Vec<ReportRow>, StorageError>;

    /// List reports filed by a single user (newest first).
    async fn list_for_reporter(
        &self,
        reporter_id: Uuid,
        limit: i64,
    ) -> Result<Vec<ReportRow>, StorageError>;

    /// Fetch one report by id.
    async fn get(&self, id: Uuid) -> Result<Option<ReportRow>, StorageError>;

    /// Resolve a report per [`ResolveSpec`]. Submissions
    /// can't be soft-deleted via this path
    /// (`spec.also_delete_target` returns an
    /// `InvalidArgument` error for a submission target so
    /// the gateway maps it to a 4xx).
    async fn resolve(
        &self,
        id: Uuid,
        moderator_id: Uuid,
        spec: ResolveSpec<'_>,
        now: DateTime<Utc>,
    ) -> Result<Option<ReportRow>, StorageError>;
}

#[async_trait]
impl ReportRepo for Storage {
    async fn insert(
        &self,
        new: NewReport,
        auto_hide_threshold: i64,
    ) -> Result<InsertOutcome, StorageError> {
        let mut tx = self.pool().begin().await?;
        // Insert the report. ON CONFLICT keeps the
        // existing row so the moderator queue stays
        // de-duped, and the CASE WHEN guards make a
        // resolved row immutable from this path — a
        // reporter re-filing after a moderator already
        // dispositioned can't rewrite the audit trail.
        // The `DO UPDATE` body still executes
        // unconditionally on conflict so RETURNING
        // always emits the row id.
        // `xmax = 0` on the RETURNING row means a fresh
        // INSERT; `xmax != 0` means the ON CONFLICT DO
        // UPDATE path fired. Postgres exposes the system
        // column directly, so we get the created vs
        // upserted signal without an extra round trip.
        let inserted = sqlx::query(
            "INSERT INTO reports
                 (reporter_id, target_kind, target_id, reason_category, body, created_at)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (reporter_id, target_kind, target_id) DO UPDATE
                SET reason_category = CASE WHEN reports.resolved_at IS NULL
                                           THEN EXCLUDED.reason_category
                                           ELSE reports.reason_category
                                      END,
                    body            = CASE WHEN reports.resolved_at IS NULL
                                           THEN EXCLUDED.body
                                           ELSE reports.body
                                      END
             RETURNING id, (xmax = 0) AS created",
        )
        .bind(new.reporter_id)
        .bind(new.target_kind.as_str())
        .bind(new.target_id)
        .bind(new.reason.as_str())
        .bind(&new.body)
        .bind(new.created_at)
        .fetch_one(&mut *tx)
        .await?;
        let report_id: Uuid = inserted.try_get("id")?;
        let created: bool = inserted.try_get("created")?;
        // Count UNRESOLVED reporters on this target. Once
        // a moderator dispositions a report, it stops
        // contributing to the auto-hide threshold —
        // otherwise the historical count would stay ≥
        // threshold forever and re-hide content on every
        // new (possibly innocuous) report after a Keep.
        // NULL reporter_id (deleted user) still counts;
        // moderation signal trumps user-account churn.
        let count_row = sqlx::query(
            "SELECT COUNT(*)::BIGINT AS n
               FROM reports
              WHERE target_kind = $1 AND target_id = $2
                AND resolved_at IS NULL",
        )
        .bind(new.target_kind.as_str())
        .bind(new.target_id)
        .fetch_one(&mut *tx)
        .await?;
        let reporter_count: i64 = count_row.try_get("n")?;
        // Flip `hidden_at` when the threshold is crossed.
        // `WHERE hidden_at IS NULL` ensures we only count
        // it as "freshly hidden" on the transition; a
        // re-filing past the threshold is a no-op for the
        // hide column.
        let freshly_hidden = if reporter_count >= auto_hide_threshold {
            let table = match new.target_kind {
                ReportTargetKind::Comment => "comments",
                ReportTargetKind::Submission => "submissions",
            };
            let sql = format!(
                "UPDATE {table} SET hidden_at = $1
                  WHERE id = $2 AND hidden_at IS NULL"
            );
            let updated = sqlx::query(&sql)
                .bind(new.created_at)
                .bind(new.target_id)
                .execute(&mut *tx)
                .await?;
            updated.rows_affected() > 0
        } else {
            false
        };
        tx.commit().await?;
        Ok(InsertOutcome {
            report_id,
            created,
            reporter_count,
            freshly_hidden,
        })
    }

    async fn list_open(
        &self,
        before: Option<DateTime<Utc>>,
        limit: i64,
    ) -> Result<Vec<ReportRow>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, reporter_id, target_kind, target_id, reason_category, body,
                    created_at, resolved_at, resolved_by, action_taken, resolution_note
               FROM reports
              WHERE resolved_at IS NULL
                AND ($1::TIMESTAMPTZ IS NULL OR created_at < $1::TIMESTAMPTZ)
              ORDER BY created_at ASC
              LIMIT $2",
        )
        .bind(before)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(ReportRow::from_row).collect()
    }

    async fn list_for_reporter(
        &self,
        reporter_id: Uuid,
        limit: i64,
    ) -> Result<Vec<ReportRow>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, reporter_id, target_kind, target_id, reason_category, body,
                    created_at, resolved_at, resolved_by, action_taken, resolution_note
               FROM reports
              WHERE reporter_id = $1
              ORDER BY created_at DESC
              LIMIT $2",
        )
        .bind(reporter_id)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(ReportRow::from_row).collect()
    }

    async fn get(&self, id: Uuid) -> Result<Option<ReportRow>, StorageError> {
        let row = sqlx::query(
            "SELECT id, reporter_id, target_kind, target_id, reason_category, body,
                    created_at, resolved_at, resolved_by, action_taken, resolution_note
               FROM reports
              WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(ReportRow::from_row).transpose()
    }

    async fn resolve(
        &self,
        id: Uuid,
        moderator_id: Uuid,
        spec: ResolveSpec<'_>,
        now: DateTime<Utc>,
    ) -> Result<Option<ReportRow>, StorageError> {
        let mut tx = self.pool().begin().await?;
        let maybe = sqlx::query(
            "UPDATE reports
                SET resolved_at     = $2,
                    resolved_by     = $3,
                    action_taken    = $4,
                    resolution_note = $5
              WHERE id = $1 AND resolved_at IS NULL
             RETURNING id, reporter_id, target_kind, target_id, reason_category, body,
                       created_at, resolved_at, resolved_by, action_taken, resolution_note",
        )
        .bind(id)
        .bind(now)
        .bind(moderator_id)
        .bind(spec.action.as_str())
        .bind(spec.resolution_note)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = maybe else {
            tx.commit().await?;
            return Ok(None);
        };
        let parsed = ReportRow::from_row(&row)?;
        if spec.also_hide_target {
            let table = match parsed.target_kind {
                ReportTargetKind::Comment => "comments",
                ReportTargetKind::Submission => "submissions",
            };
            let sql = format!(
                "UPDATE {table} SET hidden_at = $1
                  WHERE id = $2 AND hidden_at IS NULL"
            );
            sqlx::query(&sql)
                .bind(now)
                .bind(parsed.target_id)
                .execute(&mut *tx)
                .await?;
        }
        if spec.also_unhide_target {
            let table = match parsed.target_kind {
                ReportTargetKind::Comment => "comments",
                ReportTargetKind::Submission => "submissions",
            };
            let sql = format!(
                "UPDATE {table} SET hidden_at = NULL
                  WHERE id = $1 AND hidden_at IS NOT NULL"
            );
            sqlx::query(&sql)
                .bind(parsed.target_id)
                .execute(&mut *tx)
                .await?;
        }
        if spec.also_delete_target {
            match parsed.target_kind {
                ReportTargetKind::Comment => {
                    sqlx::query(
                        "UPDATE comments
                            SET deleted_at = $1, body_md = NULL
                          WHERE id = $2 AND deleted_at IS NULL",
                    )
                    .bind(now)
                    .bind(parsed.target_id)
                    .execute(&mut *tx)
                    .await?;
                }
                ReportTargetKind::Submission => {
                    // Hard-deleting submissions out of band
                    // would corrupt the moderation lifecycle.
                    // The service layer guards against this;
                    // surface it as a typed invariant
                    // failure if it ever reaches here so
                    // the gateway maps it to a 4xx.
                    return Err(StorageError::InvalidArgument(
                        "cannot delete a submission via report resolution".into(),
                    ));
                }
            }
        }
        tx.commit().await?;
        Ok(Some(parsed))
    }
}
