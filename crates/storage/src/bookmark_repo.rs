//! `bookmarks` + `collections` repositories (#5a.4).
//!
//! Two surfaces share this module because their access
//! patterns interlock — listing "my bookmarks" + listing
//! collection contents both run against the polymorphic
//! `(target_kind, target_id)` shape, and the moderator
//! audit follow-ups expect them in lockstep.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::postgres::PgRow;
use uuid::Uuid;

use crate::comment_repo::CommentTargetKind;
use crate::{Storage, StorageError};

/// Re-export so callers can use a single `BookmarkTargetKind`
/// without depending on `comment_repo`. The two enums are
/// identical at the wire level (both back the same
/// `(target_kind, target_id)` polymorphic shape across
/// `bookmarks`, `collection_items`, `comments`).
pub type BookmarkTargetKind = CommentTargetKind;

/// Row returned by [`BookmarkRepo::list_for_user`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub target_kind: BookmarkTargetKind,
    pub target_id: Uuid,
    pub created_at: DateTime<Utc>,
}

impl BookmarkRow {
    fn from_row(row: &PgRow) -> Result<Self, StorageError> {
        let kind_str: String = row.try_get("target_kind").map_err(StorageError::from)?;
        let target_kind = BookmarkTargetKind::from_wire(&kind_str).ok_or_else(|| {
            StorageError::Decode(format!(
                "unknown bookmarks.target_kind {kind_str:?} (CHECK drift?)"
            ))
        })?;
        Ok(Self {
            id: row.try_get("id").map_err(StorageError::from)?,
            user_id: row.try_get("user_id").map_err(StorageError::from)?,
            target_kind,
            target_id: row.try_get("target_id").map_err(StorageError::from)?,
            created_at: row.try_get("created_at").map_err(StorageError::from)?,
        })
    }
}

/// Outcome of [`BookmarkRepo::toggle`]. Lets the HTTP layer
/// pick the right status without an extra DB round-trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookmarkToggleOutcome {
    /// Row was inserted (heart turned on). Carries the
    /// freshly-issued bookmark id so the response can echo it.
    Bookmarked(Uuid),
    /// Row was removed (heart turned off).
    Removed,
}

#[async_trait]
pub trait BookmarkRepo: Send + Sync {
    /// Idempotent toggle. If a bookmark already exists for
    /// `(user, target_kind, target_id)` the row is deleted;
    /// otherwise a fresh row is inserted. The
    /// UNIQUE-constraint-driven path keeps both halves to a
    /// single round trip — no read-then-write race.
    async fn toggle(
        &self,
        user_id: Uuid,
        target_kind: BookmarkTargetKind,
        target_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<BookmarkToggleOutcome, StorageError>;

    /// List every bookmark the caller owns, newest first.
    /// Optional kind filter is applied at the SQL layer so a
    /// "bookmarks by kind" sidebar doesn't pull rows it'll
    /// throw away.
    async fn list_for_user(
        &self,
        user_id: Uuid,
        kind_filter: Option<BookmarkTargetKind>,
    ) -> Result<Vec<BookmarkRow>, StorageError>;

    /// Read the set of `(target_kind, target_id)` pairs the
    /// caller has bookmarked among the given inputs. Used by
    /// the catalog list pages to decorate cards with the
    /// hearted state — one query instead of N.
    async fn which_bookmarked(
        &self,
        user_id: Uuid,
        targets: &[(BookmarkTargetKind, Uuid)],
    ) -> Result<Vec<(BookmarkTargetKind, Uuid)>, StorageError>;
}

#[async_trait]
impl BookmarkRepo for Storage {
    async fn toggle(
        &self,
        user_id: Uuid,
        target_kind: BookmarkTargetKind,
        target_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<BookmarkToggleOutcome, StorageError> {
        // Insert-first, delete-on-conflict. This shape stays
        // deterministic under concurrent toggles from the
        // same user/session — the prior delete-then-insert
        // shape let two concurrent "first heart" requests
        // race past the delete (both saw zero rows) and hit
        // a UNIQUE-violation 500 on the second INSERT.
        //
        // With `ON CONFLICT DO NOTHING RETURNING id`, the
        // happy "first heart" path returns the freshly-
        // inserted UUID. The conflict path returns no rows;
        // we then DELETE the existing entry and report the
        // toggle-off outcome. Both halves run in a single
        // transaction so an interleaved rollback can't leave
        // an unbookmarked row claiming to be bookmarked.
        let mut tx = self.pool().begin().await?;
        let inserted: Option<(Uuid,)> = sqlx::query_as(
            "INSERT INTO bookmarks (user_id, target_kind, target_id, created_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (user_id, target_kind, target_id) DO NOTHING
             RETURNING id",
        )
        .bind(user_id)
        .bind(target_kind.as_str())
        .bind(target_id)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((id,)) = inserted {
            tx.commit().await?;
            return Ok(BookmarkToggleOutcome::Bookmarked(id));
        }
        sqlx::query(
            "DELETE FROM bookmarks
              WHERE user_id = $1 AND target_kind = $2 AND target_id = $3",
        )
        .bind(user_id)
        .bind(target_kind.as_str())
        .bind(target_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(BookmarkToggleOutcome::Removed)
    }

    async fn list_for_user(
        &self,
        user_id: Uuid,
        kind_filter: Option<BookmarkTargetKind>,
    ) -> Result<Vec<BookmarkRow>, StorageError> {
        let rows = if let Some(kind) = kind_filter {
            sqlx::query(
                "SELECT id, user_id, target_kind, target_id, created_at
                   FROM bookmarks
                  WHERE user_id = $1 AND target_kind = $2
                  ORDER BY created_at DESC",
            )
            .bind(user_id)
            .bind(kind.as_str())
            .fetch_all(self.pool())
            .await?
        } else {
            sqlx::query(
                "SELECT id, user_id, target_kind, target_id, created_at
                   FROM bookmarks
                  WHERE user_id = $1
                  ORDER BY created_at DESC",
            )
            .bind(user_id)
            .fetch_all(self.pool())
            .await?
        };
        rows.iter().map(BookmarkRow::from_row).collect()
    }

    async fn which_bookmarked(
        &self,
        user_id: Uuid,
        targets: &[(BookmarkTargetKind, Uuid)],
    ) -> Result<Vec<(BookmarkTargetKind, Uuid)>, StorageError> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        // Split the input into two parallel arrays so we can
        // pass them as `UNNEST($2::text[], $3::uuid[])`.
        let kinds: Vec<&'static str> = targets.iter().map(|(k, _)| k.as_str()).collect();
        let ids: Vec<Uuid> = targets.iter().map(|(_, id)| *id).collect();
        let rows = sqlx::query(
            "SELECT target_kind, target_id
               FROM bookmarks
              WHERE user_id = $1
                AND (target_kind, target_id) IN (
                    SELECT * FROM UNNEST($2::text[], $3::uuid[])
                )",
        )
        .bind(user_id)
        .bind(&kinds)
        .bind(&ids)
        .fetch_all(self.pool())
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let kind_str: String = row.try_get("target_kind").map_err(StorageError::from)?;
            let kind = BookmarkTargetKind::from_wire(&kind_str).ok_or_else(|| {
                StorageError::Decode(format!(
                    "unknown bookmarks.target_kind {kind_str:?} (CHECK drift?)"
                ))
            })?;
            let id: Uuid = row.try_get("target_id").map_err(StorageError::from)?;
            out.push((kind, id));
        }
        Ok(out)
    }
}

// === Collections ===

/// Row returned by [`CollectionRepo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl CollectionRow {
    fn from_row(row: &PgRow) -> Result<Self, StorageError> {
        Ok(Self {
            id: row.try_get("id").map_err(StorageError::from)?,
            user_id: row.try_get("user_id").map_err(StorageError::from)?,
            name: row.try_get("name").map_err(StorageError::from)?,
            description: row.try_get("description").map_err(StorageError::from)?,
            created_at: row.try_get("created_at").map_err(StorageError::from)?,
            updated_at: row.try_get("updated_at").map_err(StorageError::from)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionItemRow {
    pub collection_id: Uuid,
    pub target_kind: BookmarkTargetKind,
    pub target_id: Uuid,
    pub added_at: DateTime<Utc>,
}

impl CollectionItemRow {
    fn from_row(row: &PgRow) -> Result<Self, StorageError> {
        let kind_str: String = row.try_get("target_kind").map_err(StorageError::from)?;
        let target_kind = BookmarkTargetKind::from_wire(&kind_str).ok_or_else(|| {
            StorageError::Decode(format!(
                "unknown collection_items.target_kind {kind_str:?} (CHECK drift?)"
            ))
        })?;
        Ok(Self {
            collection_id: row.try_get("collection_id").map_err(StorageError::from)?,
            target_kind,
            target_id: row.try_get("target_id").map_err(StorageError::from)?,
            added_at: row.try_get("added_at").map_err(StorageError::from)?,
        })
    }
}

/// Input to [`CollectionRepo::insert`].
#[derive(Debug, Clone)]
pub struct NewCollection {
    pub user_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[async_trait]
pub trait CollectionRepo: Send + Sync {
    /// Create a new collection. Returns
    /// `Err(UniqueViolation)` when the user already has a
    /// collection with the same name — caller folds into the
    /// 409 response.
    async fn insert(&self, new: NewCollection) -> Result<CollectionRow, StorageError>;

    /// List every collection the caller owns, newest first.
    async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<CollectionRow>, StorageError>;

    /// Fetch a single collection, ownership-scoped — a caller
    /// reading someone else's collection gets `Ok(None)`.
    async fn get_for_user(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<CollectionRow>, StorageError>;

    /// Rename + optionally update description. Ownership-
    /// scoped. `description` follows PATCH semantics:
    ///
    ///   * `None` → preserve the existing value.
    ///   * `Some(None)` → set the column to SQL NULL.
    ///   * `Some(Some("…"))` → replace with the new string.
    ///
    /// The conditional update uses a single `CASE` so a
    /// rename + clear race can't interleave a separate
    /// description write. The `Option<Option<T>>` shape is
    /// the canonical three-state PATCH encoding; clippy's
    /// `option_option` lint flags it by default — here it's
    /// a deliberate protocol detail.
    #[allow(clippy::option_option)]
    async fn update(
        &self,
        id: Uuid,
        user_id: Uuid,
        name: &str,
        description: Option<Option<&str>>,
        now: DateTime<Utc>,
    ) -> Result<Option<CollectionRow>, StorageError>;

    /// Delete the collection. Ownership-scoped. CASCADE on
    /// `collection_items` takes the items with it.
    async fn delete(&self, id: Uuid, user_id: Uuid) -> Result<bool, StorageError>;

    /// List items in a collection. Ownership-scoped — a
    /// non-owning caller gets `Ok(None)` so the gateway can
    /// 404 the read.
    async fn list_items(
        &self,
        collection_id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<Vec<CollectionItemRow>>, StorageError>;

    /// Add an item to a collection. Idempotent (the composite
    /// PK absorbs the conflict). Ownership-scoped.
    ///
    /// Returns `Ok(true)` when a fresh row landed.
    /// Returns `Ok(false)` when no row was inserted — which
    /// folds together two cases:
    ///
    ///   1. The collection isn't owned by the caller (or
    ///      doesn't exist). Caller should 404.
    ///   2. The `(target_kind, target_id)` pair is already
    ///      in the collection. Caller should 204 / treat
    ///      as a successful idempotent add.
    ///
    /// To distinguish them, the gateway re-reads the
    /// collection via `get_for_user`; an owner-confirmed
    /// `Ok(false)` is case 2, an `Ok(None)` is case 1.
    async fn add_item(
        &self,
        collection_id: Uuid,
        user_id: Uuid,
        target_kind: BookmarkTargetKind,
        target_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<bool, StorageError>;

    /// Remove an item from a collection. Ownership-scoped.
    /// Returns `Ok(false)` when nothing was deleted (either
    /// not present or not yours).
    async fn remove_item(
        &self,
        collection_id: Uuid,
        user_id: Uuid,
        target_kind: BookmarkTargetKind,
        target_id: Uuid,
    ) -> Result<bool, StorageError>;
}

#[async_trait]
impl CollectionRepo for Storage {
    async fn insert(&self, new: NewCollection) -> Result<CollectionRow, StorageError> {
        let row = sqlx::query(
            "INSERT INTO collections (user_id, name, description, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $4)
             RETURNING id, user_id, name, description, created_at, updated_at",
        )
        .bind(new.user_id)
        .bind(&new.name)
        .bind(new.description.as_deref())
        .bind(new.created_at)
        .fetch_one(self.pool())
        .await
        .map_err(crate::sqlx_errors::map_unique_violation)?;
        CollectionRow::from_row(&row)
    }

    async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<CollectionRow>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, user_id, name, description, created_at, updated_at
               FROM collections
              WHERE user_id = $1
              ORDER BY created_at DESC",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(CollectionRow::from_row).collect()
    }

    async fn get_for_user(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<CollectionRow>, StorageError> {
        let maybe = sqlx::query(
            "SELECT id, user_id, name, description, created_at, updated_at
               FROM collections
              WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(self.pool())
        .await?;
        maybe.as_ref().map(CollectionRow::from_row).transpose()
    }

    async fn update(
        &self,
        id: Uuid,
        user_id: Uuid,
        name: &str,
        description: Option<Option<&str>>,
        now: DateTime<Utc>,
    ) -> Result<Option<CollectionRow>, StorageError> {
        // `description` is `Option<Option<&str>>`:
        //   None             → preserve (CASE → existing).
        //   Some(None)       → set NULL.
        //   Some(Some(s))    → replace with s.
        // Encode that as one boolean ($4) + one nullable
        // payload ($5) so the SQL is a single UPDATE.
        let (should_set_desc, desc_value): (bool, Option<&str>) = match description {
            None => (false, None),
            Some(value) => (true, value),
        };
        let maybe = sqlx::query(
            "UPDATE collections
                SET name = $3,
                    description = CASE WHEN $4::boolean THEN $5::text ELSE description END,
                    updated_at = GREATEST(updated_at, $6)
              WHERE id = $1 AND user_id = $2
             RETURNING id, user_id, name, description, created_at, updated_at",
        )
        .bind(id)
        .bind(user_id)
        .bind(name)
        .bind(should_set_desc)
        .bind(desc_value)
        .bind(now)
        .fetch_optional(self.pool())
        .await
        .map_err(crate::sqlx_errors::map_unique_violation)?;
        maybe.as_ref().map(CollectionRow::from_row).transpose()
    }

    async fn delete(&self, id: Uuid, user_id: Uuid) -> Result<bool, StorageError> {
        let result = sqlx::query("DELETE FROM collections WHERE id = $1 AND user_id = $2")
            .bind(id)
            .bind(user_id)
            .execute(self.pool())
            .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn list_items(
        &self,
        collection_id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<Vec<CollectionItemRow>>, StorageError> {
        // Ownership-scoped via a JOIN — the common
        // "non-empty + owned" path stays a single round
        // trip. Only the empty result needs a second probe
        // to tell "empty collection (owned)" apart from
        // "missing / not yours".
        let rows = sqlx::query(
            "SELECT ci.collection_id, ci.target_kind, ci.target_id, ci.added_at
               FROM collection_items ci
               JOIN collections c
                 ON c.id = ci.collection_id
              WHERE ci.collection_id = $1
                AND c.user_id = $2
              ORDER BY ci.added_at DESC",
        )
        .bind(collection_id)
        .bind(user_id)
        .fetch_all(self.pool())
        .await?;
        if !rows.is_empty() {
            let parsed: Result<Vec<_>, _> = rows.iter().map(CollectionItemRow::from_row).collect();
            return Ok(Some(parsed?));
        }
        // Zero rows: re-probe the ownership-only state.
        let owns: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM collections WHERE id = $1 AND user_id = $2")
                .bind(collection_id)
                .bind(user_id)
                .fetch_one(self.pool())
                .await?;
        if owns.0 == 0 {
            Ok(None)
        } else {
            Ok(Some(Vec::new()))
        }
    }

    async fn add_item(
        &self,
        collection_id: Uuid,
        user_id: Uuid,
        target_kind: BookmarkTargetKind,
        target_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<bool, StorageError> {
        // INSERT only when the collection still belongs to
        // the caller. `WHERE EXISTS (...)` runs as a sub-query
        // so the ownership check + insert stay atomic.
        let result = sqlx::query(
            "INSERT INTO collection_items (collection_id, target_kind, target_id, added_at)
             SELECT $1, $3, $4, $5
              WHERE EXISTS (
                  SELECT 1 FROM collections
                   WHERE id = $1 AND user_id = $2
              )
             ON CONFLICT (collection_id, target_kind, target_id) DO NOTHING",
        )
        .bind(collection_id)
        .bind(user_id)
        .bind(target_kind.as_str())
        .bind(target_id)
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn remove_item(
        &self,
        collection_id: Uuid,
        user_id: Uuid,
        target_kind: BookmarkTargetKind,
        target_id: Uuid,
    ) -> Result<bool, StorageError> {
        let result = sqlx::query(
            "DELETE FROM collection_items
              WHERE collection_id = $1
                AND target_kind = $3
                AND target_id = $4
                AND EXISTS (
                    SELECT 1 FROM collections
                     WHERE id = $1 AND user_id = $2
                )",
        )
        .bind(collection_id)
        .bind(user_id)
        .bind(target_kind.as_str())
        .bind(target_id)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected() > 0)
    }
}
