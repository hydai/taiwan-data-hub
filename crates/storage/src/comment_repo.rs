//! `comments` repository (#5a.3).
//!
//! Threaded comments on community-facing surfaces
//! (datasets / tools / connectors / playgrounds). The repo
//! is thin: Markdown rendering + sanitization happen in the
//! service layer (`auth::comments`); this module only
//! handles row I/O.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::postgres::PgRow;
use uuid::Uuid;

use crate::{Storage, StorageError};

/// Surface a comment is attached to. Wire format matches the
/// `comments_target_kind_known` CHECK in migration 0015.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentTargetKind {
    Dataset,
    Tool,
    Connector,
    Playground,
}

impl CommentTargetKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dataset => "dataset",
            Self::Tool => "tool",
            Self::Connector => "connector",
            Self::Playground => "playground",
        }
    }

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

/// Input to [`CommentRepo::insert`]. The service layer is the
/// sole writer: it has already validated the body length and
/// computed the row's depth (0 for a root, 1 for a reply).
#[derive(Debug, Clone)]
pub struct NewComment {
    pub target_kind: CommentTargetKind,
    pub target_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub user_id: Uuid,
    /// Author-supplied Markdown source. The service layer
    /// renders this to sanitized HTML on read; the column
    /// stays small and an XSS fix is a code deploy.
    pub body_md: String,
    /// Thread depth: 0 = root, 1 = reply. Enforced both at
    /// the service layer (refuses a reply on a depth=1
    /// parent) and by the DB CHECK `comments_depth_max_two`.
    pub depth: i16,
    pub created_at: DateTime<Utc>,
}

/// Row shape returned to callers. `body_md` is `None` on a
/// soft-deleted row; the rendering layer emits a tombstone
/// (`"[deleted]"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentRow {
    pub id: Uuid,
    pub target_kind: CommentTargetKind,
    pub target_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub body_md: Option<String>,
    pub depth: i16,
    pub created_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
}

impl CommentRow {
    fn from_row(row: &PgRow) -> Result<Self, StorageError> {
        let kind_str: String = row.try_get("target_kind").map_err(StorageError::from)?;
        let target_kind = CommentTargetKind::from_wire(&kind_str).ok_or_else(|| {
            StorageError::Decode(format!(
                "unknown comments.target_kind {kind_str:?} (CHECK constraint drift?)"
            ))
        })?;
        Ok(Self {
            id: row.try_get("id").map_err(StorageError::from)?,
            target_kind,
            target_id: row.try_get("target_id").map_err(StorageError::from)?,
            parent_id: row.try_get("parent_id").map_err(StorageError::from)?,
            user_id: row.try_get("user_id").map_err(StorageError::from)?,
            body_md: row.try_get("body_md").map_err(StorageError::from)?,
            depth: row.try_get("depth").map_err(StorageError::from)?,
            created_at: row.try_get("created_at").map_err(StorageError::from)?,
            edited_at: row.try_get("edited_at").map_err(StorageError::from)?,
            deleted_at: row.try_get("deleted_at").map_err(StorageError::from)?,
        })
    }
}

#[async_trait]
pub trait CommentRepo: Send + Sync {
    /// Persist a new comment. Returns the assigned UUID. The
    /// service layer is the only writer; it has already
    /// validated the body, computed `depth`, and (for replies)
    /// confirmed the parent row's depth is 0.
    async fn insert(&self, new: NewComment) -> Result<Uuid, StorageError>;

    /// Fetch a single comment by id (regardless of deletion
    /// state, so the gateway can return a stable 404 vs 410
    /// shape). Soft-deleted rows are returned with
    /// `body_md = None`.
    async fn get(&self, id: Uuid) -> Result<Option<CommentRow>, StorageError>;

    /// List every comment attached to `(target_kind, target_id)`
    /// in `created_at ASC` (oldest-first). The composite
    /// `comments_target_idx` index serves this exact query
    /// shape. Soft-deleted rows are included so the thread
    /// structure (parent → reply) stays intact; the caller's
    /// rendering layer emits a tombstone for them.
    async fn list_for_target(
        &self,
        target_kind: CommentTargetKind,
        target_id: Uuid,
    ) -> Result<Vec<CommentRow>, StorageError>;

    /// Author-side edit. Updates `body_md` + `edited_at = now`
    /// only when the row's `user_id = $author`, the row is not
    /// soft-deleted, AND `now - created_at <= edit_window`. The
    /// edit-window check is enforced at the SQL layer so a
    /// race between the service's pre-read and the UPDATE
    /// can't slip past the cutoff. Returns `Ok(None)` for any
    /// failed predicate — caller maps to 403 / 410 / 409.
    async fn edit(
        &self,
        id: Uuid,
        author_id: Uuid,
        new_body: &str,
        edit_window_secs: i64,
        now: DateTime<Utc>,
    ) -> Result<Option<CommentRow>, StorageError>;

    /// Author-side soft-delete. Sets `deleted_at = now` and
    /// drops `body_md` to NULL, preserving the row + its
    /// thread continuity. Idempotent: a second call returns
    /// `Ok(None)`. Ownership-scoped — a non-author call also
    /// returns `Ok(None)`.
    async fn delete(
        &self,
        id: Uuid,
        author_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<Option<CommentRow>, StorageError>;
}

#[async_trait]
impl CommentRepo for Storage {
    async fn insert(&self, new: NewComment) -> Result<Uuid, StorageError> {
        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO comments
                (target_kind, target_id, parent_id, user_id, body_md, depth, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             RETURNING id",
        )
        .bind(new.target_kind.as_str())
        .bind(new.target_id)
        .bind(new.parent_id)
        .bind(new.user_id)
        .bind(&new.body_md)
        .bind(new.depth)
        .bind(new.created_at)
        .fetch_one(self.pool())
        .await?;
        Ok(row.0)
    }

    async fn get(&self, id: Uuid) -> Result<Option<CommentRow>, StorageError> {
        let maybe = sqlx::query(
            "SELECT id, target_kind, target_id, parent_id, user_id, body_md, depth,
                    created_at, edited_at, deleted_at
               FROM comments WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        maybe.as_ref().map(CommentRow::from_row).transpose()
    }

    async fn list_for_target(
        &self,
        target_kind: CommentTargetKind,
        target_id: Uuid,
    ) -> Result<Vec<CommentRow>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, target_kind, target_id, parent_id, user_id, body_md, depth,
                    created_at, edited_at, deleted_at
               FROM comments
              WHERE target_kind = $1 AND target_id = $2
              ORDER BY created_at ASC",
        )
        .bind(target_kind.as_str())
        .bind(target_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(CommentRow::from_row).collect()
    }

    async fn edit(
        &self,
        id: Uuid,
        author_id: Uuid,
        new_body: &str,
        edit_window_secs: i64,
        now: DateTime<Utc>,
    ) -> Result<Option<CommentRow>, StorageError> {
        // The edit-window guard lives in the predicate so a
        // race between the service's check and the UPDATE
        // can't slip past the cutoff. `$4 * INTERVAL '1 second'`
        // builds the interval from the integer-second arg at
        // SQL time, avoiding an f64 bind (which clippy's
        // `cast_precision_loss` flags on i64 → f64).
        let maybe = sqlx::query(
            "UPDATE comments
                SET body_md = $3,
                    edited_at = $5
              WHERE id = $1
                AND user_id = $2
                AND deleted_at IS NULL
                AND $5 - created_at <= $4::bigint * INTERVAL '1 second'
             RETURNING id, target_kind, target_id, parent_id, user_id, body_md, depth,
                       created_at, edited_at, deleted_at",
        )
        .bind(id)
        .bind(author_id)
        .bind(new_body)
        .bind(edit_window_secs)
        .bind(now)
        .fetch_optional(self.pool())
        .await?;
        maybe.as_ref().map(CommentRow::from_row).transpose()
    }

    async fn delete(
        &self,
        id: Uuid,
        author_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<Option<CommentRow>, StorageError> {
        let maybe = sqlx::query(
            "UPDATE comments
                SET deleted_at = $3,
                    body_md = NULL
              WHERE id = $1
                AND user_id = $2
                AND deleted_at IS NULL
             RETURNING id, target_kind, target_id, parent_id, user_id, body_md, depth,
                       created_at, edited_at, deleted_at",
        )
        .bind(id)
        .bind(author_id)
        .bind(now)
        .fetch_optional(self.pool())
        .await?;
        maybe.as_ref().map(CommentRow::from_row).transpose()
    }
}
