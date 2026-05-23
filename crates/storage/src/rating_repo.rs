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
        // Single round trip: LEFT JOIN the cached aggregate
        // and the viewer's own rating onto a one-row anchor
        // (so the row is always returned even when the
        // aggregate is missing). The `r.user_id IS NULL`
        // guard makes the JOIN drop the row when the caller
        // is anonymous, leaving `viewer_score` NULL.
        let row = sqlx::query(
            "SELECT a.target_kind     AS agg_target_kind,
                    a.target_id       AS agg_target_id,
                    a.avg_score,
                    a.rating_count,
                    a.last_refreshed_at,
                    r.score           AS viewer_score
               FROM (SELECT $1::TEXT AS k, $2::UUID AS id) AS anchor
               LEFT JOIN rating_aggregates a
                      ON a.target_kind = anchor.k AND a.target_id = anchor.id
               LEFT JOIN ratings r
                      ON $3::UUID IS NOT NULL
                     AND r.user_id     = $3::UUID
                     AND r.target_kind = anchor.k
                     AND r.target_id   = anchor.id",
        )
        .bind(target_kind.as_str())
        .bind(target_id)
        .bind(viewer_id)
        .fetch_one(self.pool())
        .await?;
        let rating_count: Option<i32> = row.try_get("rating_count")?;
        let aggregate = if let Some(count) = rating_count {
            let kind_str: String = row.try_get("agg_target_kind")?;
            let agg_kind = RatingTargetKind::from_wire(&kind_str).ok_or_else(|| {
                StorageError::Decode(format!(
                    "unknown rating_aggregates.target_kind {kind_str:?} (CHECK drift?)"
                ))
            })?;
            Some(RatingAggregateRow {
                target_kind: agg_kind,
                target_id: row.try_get("agg_target_id")?,
                avg_score: row.try_get("avg_score")?,
                rating_count: count,
                last_refreshed_at: row.try_get("last_refreshed_at")?,
            })
        } else {
            None
        };
        let viewer_score: Option<i16> = row.try_get("viewer_score")?;
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
/// **Concurrency**: under `PostgreSQL`'s default `READ`
/// `COMMITTED`, two concurrent writers to the same target
/// can each compute the aggregate from a snapshot taken
/// *before* the other's `ratings` row is visible. Without a
/// barrier, the second-to-write transaction's aggregate
/// row overwrites the first's using stale data, leaving
/// the cache off by one row. We take a per-target advisory
/// lock at transaction scope (released on COMMIT/ROLLBACK)
/// so the `AVG`/`COUNT` recompute runs serially per target.
/// Different targets stay in parallel — the lock key
/// derives from `(target_kind, target_id)`.
async fn refresh_aggregate(
    conn: &mut sqlx::PgConnection,
    target_kind: RatingTargetKind,
    target_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), StorageError> {
    // `pg_advisory_xact_lock(bigint)` — single 64-bit key.
    // The 2-arg `pg_advisory_xact_lock(int, int)` form is
    // *32-bit* ints (int4), so binding `i64` here would
    // either fail function resolution or overflow on cast.
    // We pack the kind discriminator into the top 8 bits
    // and the UUID-folded entropy into the low 56 so
    // unrelated `(kind, id)` pairs almost never collide
    // (the kind byte alone keeps the four polymorphic
    // kinds disjoint, and 56 bits of UUID entropy makes
    // intra-kind collisions astronomically rare).
    let lock_key = advisory_lock_key(target_kind, target_id);
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(lock_key)
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
/// four kinds map to four distinct u8 values that occupy the
/// top byte of the 64-bit lock key so unrelated kinds don't
/// queue on each other.
fn hash_kind(kind: RatingTargetKind) -> u8 {
    match kind {
        RatingTargetKind::Dataset => 1,
        RatingTargetKind::Tool => 2,
        RatingTargetKind::Connector => 3,
        RatingTargetKind::Playground => 4,
    }
}

/// 64-bit composite key for `pg_advisory_xact_lock(bigint)`.
/// Layout: `[kind:u8] [uuid_fold:u56]` where the kind byte
/// occupies the most-significant byte so the four kinds are
/// trivially disjoint, and the UUID fold (XOR of the two
/// halves, masked to 56 bits) supplies enough entropy that
/// intra-kind collisions are astronomically rare. Advisory
/// lock keys don't need cryptographic uniqueness — only
/// "distinct enough that unrelated writers don't serialise".
/// `from_ne_bytes` reinterprets the `u64` bit pattern as
/// `i64` without the `cast_possible_wrap` concern.
fn advisory_lock_key(kind: RatingTargetKind, target_id: Uuid) -> i64 {
    let (hi, lo) = target_id.as_u64_pair();
    let uuid_fold = (hi ^ lo) & 0x00FF_FFFF_FFFF_FFFF;
    let kind_byte = u64::from(hash_kind(kind)) << 56;
    i64::from_ne_bytes((kind_byte | uuid_fold).to_ne_bytes())
}
