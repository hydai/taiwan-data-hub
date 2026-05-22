//! `rate_limit_counters` repository (#4.7).
//!
//! Fixed-window counter storage that backs `auth::PgRateLimiter`
//! — the PG-backed implementation of the `auth::RateLimiter`
//! trait. The spec calls for `DragonflyDB` as the eventual
//! production backend (shared counter state across multi-
//! instance gateways via Redis); that impl is NOT in this PR.
//! This storage is the "small deploys without Redis" fallback
//! documented in the #4.7 definition of done, and is the
//! default production path until the `DragonflyDB`-backed
//! impl lands.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::{Storage, StorageError};

/// One read-modify-write of the counter for `(key, window)`.
///
/// Returns the count AFTER the increment so the caller can
/// compare to the limit in a single round trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CounterTick {
    /// Count of requests observed in the active window
    /// INCLUDING this one.
    pub count: i32,
    /// The window's start timestamp — the row's
    /// `window_start`. Useful for the `X-RateLimit-Reset`
    /// header (caller computes `window_start + window_size`
    /// to get the reset time).
    pub window_start: DateTime<Utc>,
}

#[async_trait]
pub trait RateLimitRepo: Send + Sync {
    /// Atomically check + bump the counter for `key` against
    /// the window starting at `window_start`.
    ///
    /// If a row already exists for `key`:
    ///
    /// - Same window (`row.window_start >= window_start`):
    ///   `count = count + 1`, `window_start` unchanged.
    /// - Older window (`row.window_start < window_start`):
    ///   `count = 1`, `window_start = window_start` (the new
    ///   window resets the counter atomically with the read).
    ///
    /// If no row exists: insert with `count = 1, window_start =
    /// window_start`. Returns the post-increment count and the
    /// active window's start.
    async fn check_and_increment(
        &self,
        key: &str,
        window_start: DateTime<Utc>,
    ) -> Result<CounterTick, StorageError>;

    /// Delete rows whose `window_start` is older than `cutoff`.
    /// Used by the GC job to bound eventual table size. Returns
    /// the number of rows removed.
    async fn sweep_expired(&self, cutoff: DateTime<Utc>) -> Result<u64, StorageError>;
}

#[async_trait]
impl RateLimitRepo for Storage {
    async fn check_and_increment(
        &self,
        key: &str,
        window_start: DateTime<Utc>,
    ) -> Result<CounterTick, StorageError> {
        // `INSERT ... ON CONFLICT DO UPDATE ... RETURNING` does
        // the read-and-bump in one statement. The CASE in the
        // SET clause is the fixed-window reset: when the
        // existing row's `window_start` is older than the new
        // one, treat this request as the first in a fresh
        // window; otherwise just increment.
        //
        // Note we compare `EXCLUDED.window_start` against the
        // ROW's current `window_start` — that's the "new vs.
        // old window" decision. `EXCLUDED` carries what we
        // tried to insert; the row's current values are the
        // bare column names without a prefix in the `DO
        // UPDATE` branch.
        let row = sqlx::query_as::<_, (i32, DateTime<Utc>)>(
            "INSERT INTO rate_limit_counters (key, window_start, count)
             VALUES ($1, $2, 1)
             ON CONFLICT (key) DO UPDATE
               SET count = CASE
                     WHEN rate_limit_counters.window_start < EXCLUDED.window_start THEN 1
                     ELSE rate_limit_counters.count + 1
                   END,
                   window_start = CASE
                     WHEN rate_limit_counters.window_start < EXCLUDED.window_start
                       THEN EXCLUDED.window_start
                     ELSE rate_limit_counters.window_start
                   END
             RETURNING count, window_start",
        )
        .bind(key)
        .bind(window_start)
        .fetch_one(self.pool())
        .await?;
        Ok(CounterTick {
            count: row.0,
            window_start: row.1,
        })
    }

    async fn sweep_expired(&self, cutoff: DateTime<Utc>) -> Result<u64, StorageError> {
        let result = sqlx::query("DELETE FROM rate_limit_counters WHERE window_start < $1")
            .bind(cutoff)
            .execute(self.pool())
            .await?;
        Ok(result.rows_affected())
    }
}
