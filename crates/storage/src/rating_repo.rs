//! `ratings` + `rating_aggregates` repository (#5a.5).
//!
//! Upsert + withdraw paths refresh the cached aggregate in
//! the same transaction so the dataset-page read can join a
//! single row instead of running an aggregation per render.
//! The nightly cron-driven full recompute lives in M5b; until
//! it lands, the on-write refresh is the only source.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::postgres::PgRow;
use uuid::Uuid;

use crate::comment_repo::CommentTargetKind;
use crate::{Storage, StorageError};

/// Reuse the polymorphic kind already established for
/// `comments` + `bookmarks` so the four community-facing
/// surfaces share one wire format.
pub type RatingTargetKind = CommentTargetKind;

/// Row in `ratings` (one per user/target).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RatingRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub target_kind: RatingTargetKind,
    pub target_id: Uuid,
    pub score: i16,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl RatingRow {
    fn from_row(row: &PgRow) -> Result<Self, StorageError> {
        let kind_str: String = row.try_get("target_kind").map_err(StorageError::from)?;
        let target_kind = RatingTargetKind::from_wire(&kind_str).ok_or_else(|| {
            StorageError::Decode(format!(
                "unknown ratings.target_kind {kind_str:?} (CHECK drift?)"
            ))
        })?;
        Ok(Self {
            id: row.try_get("id").map_err(StorageError::from)?,
            user_id: row.try_get("user_id").map_err(StorageError::from)?,
            target_kind,
            target_id: row.try_get("target_id").map_err(StorageError::from)?,
            score: row.try_get("score").map_err(StorageError::from)?,
            created_at: row.try_get("created_at").map_err(StorageError::from)?,
            updated_at: row.try_get("updated_at").map_err(StorageError::from)?,
        })
    }
}

/// Cached aggregate. A missing row and a `count == 0` row are
/// semantically equivalent ("no ratings yet"); the gateway
/// surfaces both as `{ count: 0, avg: null }` so the
/// frontend can treat them identically.
#[derive(Debug, Clone, PartialEq)]
pub struct RatingAggregateRow {
    pub target_kind: RatingTargetKind,
    pub target_id: Uuid,
    pub avg_score: f64,
    pub rating_count: i32,
    pub last_refreshed_at: DateTime<Utc>,
}

impl RatingAggregateRow {
    fn from_row(row: &PgRow) -> Result<Self, StorageError> {
        let kind_str: String = row.try_get("target_kind").map_err(StorageError::from)?;
        let target_kind = RatingTargetKind::from_wire(&kind_str).ok_or_else(|| {
            StorageError::Decode(format!(
                "unknown rating_aggregates.target_kind {kind_str:?} (CHECK drift?)"
            ))
        })?;
        Ok(Self {
            target_kind,
            target_id: row.try_get("target_id").map_err(StorageError::from)?,
            avg_score: row.try_get("avg_score").map_err(StorageError::from)?,
            rating_count: row.try_get("rating_count").map_err(StorageError::from)?,
            last_refreshed_at: row
                .try_get("last_refreshed_at")
                .map_err(StorageError::from)?,
        })
    }
}

/// Score paired with the user's own row (when present) +
/// the public aggregate. The HTTP layer assembles a single
/// JSON response from this.
#[derive(Debug, Clone, PartialEq)]
pub struct RatingView {
    pub aggregate: Option<RatingAggregateRow>,
    pub viewer_score: Option<i16>,
}

#[async_trait]
pub trait RatingRepo: Send + Sync {
    /// Idempotent upsert. Score is clamped to 1..=5 at the
    /// service layer; the CHECK constraint enforces it again
    /// here. Refreshes the matching `rating_aggregates` row
    /// in the same transaction.
    async fn upsert(
        &self,
        user_id: Uuid,
        target_kind: RatingTargetKind,
        target_id: Uuid,
        score: i16,
        now: DateTime<Utc>,
    ) -> Result<RatingRow, StorageError>;

    /// Delete the caller's rating for `(target_kind,
    /// target_id)`. Returns `Ok(true)` when a row was
    /// removed, `Ok(false)` when there was nothing to
    /// remove. The matching aggregate is refreshed even on
    /// `Ok(false)` so a no-op delete is still safe to
    /// retry against a stale aggregate.
    async fn withdraw(
        &self,
        user_id: Uuid,
        target_kind: RatingTargetKind,
        target_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<bool, StorageError>;

    /// Single combined read: aggregate + the viewer's own
    /// score if signed-in. `viewer_id == None` for anonymous
    /// reads (still returns the aggregate). One round trip
    /// keeps the dataset-page server-load cheap.
    async fn view(
        &self,
        target_kind: RatingTargetKind,
        target_id: Uuid,
        viewer_id: Option<Uuid>,
    ) -> Result<RatingView, StorageError>;

    /// Bulk aggregate fetch for catalog list pages — one
    /// query, N cards. Targets without any ratings are
    /// returned as `Ok(empty Vec)` (not zero-row entries),
    /// so the caller renders "no ratings yet" by absence.
    async fn aggregates_for(
        &self,
        targets: &[(RatingTargetKind, Uuid)],
    ) -> Result<Vec<RatingAggregateRow>, StorageError>;
}

#[async_trait]
impl RatingRepo for Storage {
    async fn upsert(
        &self,
        user_id: Uuid,
        target_kind: RatingTargetKind,
        target_id: Uuid,
        score: i16,
        now: DateTime<Utc>,
    ) -> Result<RatingRow, StorageError> {
        // The whole operation is one transaction so a
        // concurrent reader can't see a rating without its
        // matching aggregate refresh.
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query(
            "INSERT INTO ratings (user_id, target_kind, target_id, score, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $5)
             ON CONFLICT (user_id, target_kind, target_id) DO UPDATE
                SET score = EXCLUDED.score,
                    updated_at = EXCLUDED.updated_at
             RETURNING id, user_id, target_kind, target_id, score, created_at, updated_at",
        )
        .bind(user_id)
        .bind(target_kind.as_str())
        .bind(target_id)
        .bind(score)
        .bind(now)
        .fetch_one(&mut *tx)
        .await
        .map_err(crate::sqlx_errors::map_unique_violation)?;
        refresh_aggregate(&mut tx, target_kind, target_id, now).await?;
        tx.commit().await?;
        RatingRow::from_row(&row)
    }

    async fn withdraw(
        &self,
        user_id: Uuid,
        target_kind: RatingTargetKind,
        target_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<bool, StorageError> {
        let mut tx = self.pool().begin().await?;
        let result = sqlx::query(
            "DELETE FROM ratings
              WHERE user_id = $1 AND target_kind = $2 AND target_id = $3",
        )
        .bind(user_id)
        .bind(target_kind.as_str())
        .bind(target_id)
        .execute(&mut *tx)
        .await?;
        refresh_aggregate(&mut tx, target_kind, target_id, now).await?;
        tx.commit().await?;
        Ok(result.rows_affected() > 0)
    }

    async fn view(
        &self,
        target_kind: RatingTargetKind,
        target_id: Uuid,
        viewer_id: Option<Uuid>,
    ) -> Result<RatingView, StorageError> {
        let aggregate = sqlx::query(
            "SELECT target_kind, target_id, avg_score, rating_count, last_refreshed_at
               FROM rating_aggregates
              WHERE target_kind = $1 AND target_id = $2",
        )
        .bind(target_kind.as_str())
        .bind(target_id)
        .fetch_optional(self.pool())
        .await?
        .map(|r| RatingAggregateRow::from_row(&r))
        .transpose()?;
        let viewer_score = match viewer_id {
            None => None,
            Some(uid) => {
                sqlx::query_scalar::<_, i16>(
                    "SELECT score FROM ratings
                  WHERE user_id = $1 AND target_kind = $2 AND target_id = $3",
                )
                .bind(uid)
                .bind(target_kind.as_str())
                .bind(target_id)
                .fetch_optional(self.pool())
                .await?
            }
        };
        Ok(RatingView {
            aggregate,
            viewer_score,
        })
    }

    async fn aggregates_for(
        &self,
        targets: &[(RatingTargetKind, Uuid)],
    ) -> Result<Vec<RatingAggregateRow>, StorageError> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        let kinds: Vec<&str> = targets.iter().map(|(k, _)| k.as_str()).collect();
        let ids: Vec<Uuid> = targets.iter().map(|(_, id)| *id).collect();
        let rows = sqlx::query(
            "SELECT target_kind, target_id, avg_score, rating_count, last_refreshed_at
               FROM rating_aggregates
               JOIN UNNEST($1::TEXT[], $2::UUID[]) AS t(kind, id)
                 ON target_kind = t.kind AND target_id = t.id
              WHERE rating_count > 0",
        )
        .bind(&kinds)
        .bind(&ids)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(RatingAggregateRow::from_row).collect()
    }
}

/// Recompute the cached aggregate for `(target_kind,
/// target_id)` from the current `ratings` rows. Called in
/// the same transaction as the originating insert/update/
/// delete so an external observer never sees the row and
/// its aggregate disagree.
///
/// **Concurrency**: under PostgreSQL's default READ
/// COMMITTED, two concurrent writers to the same target
/// can each compute the aggregate from a snapshot taken
/// *before* the other's `ratings` row is visible. Without a
/// barrier, the second-to-write transaction's aggregate
/// row overwrites the first's using stale data, leaving
/// the cache off by one row. We take a per-target advisory
/// lock at transaction scope (released on COMMIT/ROLLBACK)
/// so the AVG/COUNT recompute runs serially per target.
/// Different targets stay in parallel — the lock key
/// derives from `(target_kind, target_id)`.
async fn refresh_aggregate(
    conn: &mut sqlx::PgConnection,
    target_kind: RatingTargetKind,
    target_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), StorageError> {
    // `pg_advisory_xact_lock(key1::int8, key2::int8)` takes
    // two int8 keys. We fold the kind into the upper int8
    // and the target_id's low 64 bits into the lower one so
    // the lock is unique per `(kind, id)` pair without
    // colliding across kinds.
    let target_kind_hash = i64::from(hash_kind(target_kind));
    let target_id_low = target_id_low_i64(target_id);
    sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
        .bind(target_kind_hash)
        .bind(target_id_low)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "INSERT INTO rating_aggregates
             (target_kind, target_id, avg_score, rating_count, last_refreshed_at)
         SELECT $1, $2,
                COALESCE(AVG(score)::DOUBLE PRECISION, 0),
                COUNT(*)::INTEGER,
                $3
           FROM ratings
          WHERE target_kind = $1 AND target_id = $2
         ON CONFLICT (target_kind, target_id) DO UPDATE
            SET avg_score = EXCLUDED.avg_score,
                rating_count = EXCLUDED.rating_count,
                last_refreshed_at = EXCLUDED.last_refreshed_at",
    )
    .bind(target_kind.as_str())
    .bind(target_id)
    .bind(now)
    .execute(conn)
    .await?;
    Ok(())
}

/// Stable per-kind discriminator for the advisory lock. The
/// four kinds map to four distinct int4 values so concurrent
/// writers to different kinds don't serialize on the same
/// key.
fn hash_kind(kind: RatingTargetKind) -> i32 {
    match kind {
        RatingTargetKind::Dataset => 1,
        RatingTargetKind::Tool => 2,
        RatingTargetKind::Connector => 3,
        RatingTargetKind::Playground => 4,
    }
}

/// Fold a UUID's 128-bit value into the i64 the second
/// `pg_advisory_xact_lock` arg accepts. XORing the two
/// halves preserves enough entropy that collisions across
/// targets stay astronomically rare, and pg_advisory_lock
/// keys don't need to be cryptographically unique — only
/// "distinct enough that unrelated writers don't queue on
/// each other".
fn target_id_low_i64(id: Uuid) -> i64 {
    let (hi, lo) = id.as_u64_pair();
    (hi ^ lo) as i64
}
