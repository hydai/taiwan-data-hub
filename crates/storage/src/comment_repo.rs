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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;
    use testcontainers_modules::postgres::Postgres as PgContainer;
    use testcontainers_modules::testcontainers::ContainerAsync;
    use testcontainers_modules::testcontainers::ImageExt;
    use testcontainers_modules::testcontainers::runners::AsyncRunner;

    /// Spin up a Postgres container, run every migration, and
    /// return a [`Storage`] pointed at it. Mirrors the helper
    /// in `dataset_repo::tests`.
    async fn fresh_storage() -> (Storage, ContainerAsync<PgContainer>) {
        let container = PgContainer::default()
            .with_tag("18-alpine")
            .start()
            .await
            .expect("start postgres container");
        let host = container.get_host().await.expect("host");
        let port = container.get_host_port_ipv4(5432).await.expect("port");
        let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("connect");
        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .expect("migrate");
        (Storage::from_pool(pool), container)
    }

    /// Seed a minimal user row — the comments FK requires it.
    /// Argon2 hashes are irrelevant for the repo tests; any
    /// non-empty string satisfies the column.
    async fn seed_user(storage: &Storage) -> Uuid {
        let email = format!("user-{}@example.test", Uuid::now_v7());
        let (id,): (Uuid,) =
            sqlx::query_as("INSERT INTO users (email, password_hash) VALUES ($1, $2) RETURNING id")
                .bind(&email)
                .bind("placeholder")
                .fetch_one(storage.pool())
                .await
                .expect("seed user");
        id
    }

    fn target() -> (CommentTargetKind, Uuid) {
        (CommentTargetKind::Dataset, Uuid::now_v7())
    }

    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn insert_then_list_roundtrips() {
        let (storage, _c) = fresh_storage().await;
        let user_id = seed_user(&storage).await;
        let (kind, tid) = target();
        let id = storage
            .insert(NewComment {
                target_kind: kind,
                target_id: tid,
                parent_id: None,
                user_id,
                body_md: "hello".into(),
                depth: 0,
                created_at: Utc::now(),
            })
            .await
            .unwrap();
        let listed = storage.list_for_target(kind, tid).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
        assert_eq!(listed[0].body_md.as_deref(), Some("hello"));
    }

    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn edit_within_window_succeeds_past_window_fails() {
        let (storage, _c) = fresh_storage().await;
        let user_id = seed_user(&storage).await;
        let (kind, tid) = target();
        // Backdate the row by 2 seconds so a 1-second edit
        // window is provably elapsed.
        let created_at = Utc::now() - chrono::Duration::seconds(2);
        let id = storage
            .insert(NewComment {
                target_kind: kind,
                target_id: tid,
                parent_id: None,
                user_id,
                body_md: "first".into(),
                depth: 0,
                created_at,
            })
            .await
            .unwrap();
        // Edit with a 1-second window — should fail.
        let after = storage
            .edit(id, user_id, "second", 1, Utc::now())
            .await
            .unwrap();
        assert!(after.is_none(), "edit past the 1s window must be refused");
        // Edit with a generous 60-second window — should succeed.
        let after = storage
            .edit(id, user_id, "second", 60, Utc::now())
            .await
            .unwrap()
            .expect("60s window admits the edit");
        assert_eq!(after.body_md.as_deref(), Some("second"));
        assert!(after.edited_at.is_some());
    }

    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn delete_soft_drops_body_md_and_sets_deleted_at() {
        let (storage, _c) = fresh_storage().await;
        let user_id = seed_user(&storage).await;
        let (kind, tid) = target();
        let id = storage
            .insert(NewComment {
                target_kind: kind,
                target_id: tid,
                parent_id: None,
                user_id,
                body_md: "to delete".into(),
                depth: 0,
                created_at: Utc::now(),
            })
            .await
            .unwrap();
        let after = storage
            .delete(id, user_id, Utc::now())
            .await
            .unwrap()
            .expect("first delete returns the row");
        assert!(after.body_md.is_none());
        assert!(after.deleted_at.is_some());
        // Second delete is a no-op.
        let again = storage.delete(id, user_id, Utc::now()).await.unwrap();
        assert!(again.is_none());
    }

    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn depth_check_rejects_depth_2() {
        let (storage, _c) = fresh_storage().await;
        let user_id = seed_user(&storage).await;
        let (kind, tid) = target();
        let err = storage
            .insert(NewComment {
                target_kind: kind,
                target_id: tid,
                parent_id: Some(Uuid::now_v7()),
                user_id,
                body_md: "too deep".into(),
                depth: 2,
                created_at: Utc::now(),
            })
            .await
            .expect_err("CHECK rejects depth=2");
        // sqlx surfaces CHECK violations as Database errors.
        let msg = format!("{err}");
        assert!(
            msg.contains("comments_depth_max_two") || msg.contains("violates check"),
            "expected CHECK error, got {msg}"
        );
    }
}

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

    /// Fetch a single comment by id regardless of deletion
    /// state. Soft-deleted rows come back with
    /// `body_md = None` so the rendering layer can emit a
    /// tombstone — production never 404s on a soft-deleted
    /// comment because the row's id stays a valid handle for
    /// thread continuity.
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
    /// failed predicate — `auth::CommentService::edit` re-reads
    /// the row to distinguish the closed-window case (409) from
    /// the gone / not-yours case (404).
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
