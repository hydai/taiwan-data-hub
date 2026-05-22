//! Hot-dataset cache pipeline (#3.6). Periodically scans the
//! catalog for datasets that should be promoted to / demoted from
//! the parquet cache on `SeaweedFS`, and exports the current cache
//! hit ratio for telemetry.
//!
//! ## v0.1 scope
//!
//! - **Promotion**: identifies candidates and logs them via
//!   `tracing::info!` so the operator can confirm the selection
//!   rule is firing on the expected datasets. **Does not yet
//!   actually materialise** — that wiring needs the
//!   `ObjectStoreRouter` which the etl-worker doesn't currently
//!   construct. Tracked as a v0.2 follow-up.
//! - **Demotion**: fully implemented. Clears `cached` /
//!   `cache_path` on datasets that haven't been queried in
//!   `inactive_days`. The object-store lifecycle policy garbage-
//!   collects the abandoned parquet file.
//! - **Telemetry**: computes the cache hit ratio over its own
//!   short window (default 1 day via
//!   `DEFAULT_HIT_RATIO_WINDOW_DAYS`) — *not* the 7-day
//!   promotion lookback — so the gauge tracks recent traffic
//!   without being smoothed over a week. Emitted as a
//!   structured `tracing::info!`; the Prometheus exporter
//!   (#2.10) will wire this onto the
//!   `taiwan_data_hub_cache_hit_ratio` gauge.
//!
//! ## Selection rules (Definition of Done #3.6)
//!
//! Promote when:
//!  - `tier IN ('platinum', 'gold')` (editorial pin), OR
//!  - `query_rows` hit count over the last 7 days ≥ 50.
//!
//! Demote when:
//!  - Currently `cached = true`, AND
//!  - `tier NOT IN ('platinum', 'gold')` (editorial pins stay), AND
//!  - No `query_rows` call in the last 30 days.

use std::sync::Arc;

use storage::{CacheState, StorageError};
use thiserror::Error;
use uuid::Uuid;

/// Default lookback window for hot-candidate selection (per Definition of Done).
pub const DEFAULT_HOT_WINDOW_DAYS: i32 = 7;

/// Default `query_rows` hit threshold for hot-candidate selection
/// (per Definition of Done).
pub const DEFAULT_HOT_HIT_THRESHOLD: i64 = 50;

/// Default inactivity window for demotion (per Definition of Done).
pub const DEFAULT_DEMOTION_INACTIVE_DAYS: i32 = 30;

/// Window used to compute the cache hit ratio. 1 day gives a
/// sensitive moving signal without being so short that a single
/// quiet hour spikes the gauge.
pub const DEFAULT_HIT_RATIO_WINDOW_DAYS: i32 = 1;

/// Summary of one pipeline tick. Counter widths:
///  - `hot_candidate_count` / `demoted_count`: `usize` (Vec
///    lengths from the storage layer; never negative).
///  - `hit_ratio_hits` / `hit_ratio_total`: `i64` (Postgres
///    `COUNT` result type).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheTickReport {
    pub hot_candidate_count: usize,
    pub demoted_count: usize,
    pub hit_ratio_hits: i64,
    pub hit_ratio_total: i64,
}

impl CacheTickReport {
    /// Convenience: ratio of hits to total, or `None` when there
    /// were no queries in the window. Casts are lossless in
    /// practice — see [`storage::CacheHitRatio::ratio`].
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn hit_ratio(&self) -> Option<f64> {
        if self.hit_ratio_total == 0 {
            None
        } else {
            Some(self.hit_ratio_hits as f64 / self.hit_ratio_total as f64)
        }
    }
}

#[derive(Debug, Error)]
pub enum CacheTickError {
    #[error("hot-candidate query failed: {0}")]
    HotQuery(#[source] StorageError),
    #[error("cold-candidate query failed: {0}")]
    ColdQuery(#[source] StorageError),
    #[error("demote of dataset {dataset_id} failed: {source}")]
    Demote {
        dataset_id: Uuid,
        #[source]
        source: StorageError,
    },
    #[error("hit-ratio query failed: {0}")]
    HitRatio(#[source] StorageError),
}

/// Run one tick of the hot-cache pipeline. Caller wires this onto
/// the cron schedule (etl-worker `main.rs` does that for the
/// production binary; integration tests call it directly).
///
/// The function is `pub` and takes `Arc<dyn CacheState>` so a test
/// can plug in a mock implementation, and the production wiring
/// can hand in `Arc::new(storage.clone())`.
#[allow(clippy::needless_pass_by_value)] // Arc move on purpose
pub async fn run_cache_tick(
    state: Arc<dyn CacheState>,
    config: CacheTickConfig,
) -> Result<CacheTickReport, CacheTickError> {
    // 1. Find hot candidates and log them. Promotion wiring is a
    //    v0.2 follow-up — the etl-worker doesn't carry the
    //    ObjectStoreRouter that materialize_dataset needs.
    let hot = state
        .hot_candidates(config.hot_window_days, config.hot_hit_threshold)
        .await
        .map_err(CacheTickError::HotQuery)?;
    // Per-candidate detail at debug level only — a busy deployment
    // could otherwise emit hundreds of info events every 6 hours.
    // The summary line below carries the top-N for at-a-glance ops.
    for candidate in &hot {
        tracing::debug!(
            dataset_id = %candidate.id,
            slug = %candidate.slug,
            tier = %candidate.tier,
            query_hits = candidate.query_hits,
            "cache promotion candidate (v0.1: log only; v0.2 will materialise)",
        );
    }
    if !hot.is_empty() {
        // Top-5 by tier rank then hit count (the storage layer
        // already ordered them that way). Operators see what the
        // most prominent candidates were without scanning the
        // full debug log.
        let top: Vec<String> = hot
            .iter()
            .take(5)
            .map(|c| format!("{} (tier={}, hits={})", c.slug, c.tier, c.query_hits))
            .collect();
        tracing::info!(
            count = hot.len(),
            top = top.join(", "),
            "cache promotion candidates (v0.1: log only; v0.2 materialise)",
        );
    }

    // 2. Find cold candidates and demote them. This is fully
    //    implemented — clearing `cached`/`cache_path` is a single
    //    SQL UPDATE per dataset.
    //
    //    Perf note: N-update loop. At v0.1 traffic the cold list
    //    is small (a handful per 6-hour tick) so the overhead is
    //    negligible. If candidates grow into the hundreds, add
    //    `Storage::demote_datasets_bulk(&[Uuid])` doing
    //    `UPDATE ... WHERE id = ANY($1) ... RETURNING id`. v0.2
    //    work, tracked alongside the materialisation wiring.
    let cold = state
        .cold_candidates(config.demotion_inactive_days)
        .await
        .map_err(CacheTickError::ColdQuery)?;
    let mut demoted = 0_usize;
    let mut skipped_races = 0_usize;
    for candidate in &cold {
        match state.demote_dataset(candidate.id).await {
            Ok(true) => {
                demoted += 1;
                tracing::info!(
                    dataset_id = %candidate.id,
                    slug = %candidate.slug,
                    tier = %candidate.tier,
                    "demoted dataset from cache",
                );
            }
            Ok(false) => {
                // Race lost: the dataset was promoted to
                // platinum/gold (or already-uncached) between
                // the candidate scan and the UPDATE. Eligibility
                // predicate on the UPDATE handled this safely;
                // log so an operator can correlate.
                skipped_races += 1;
                tracing::info!(
                    dataset_id = %candidate.id,
                    slug = %candidate.slug,
                    "demotion skipped: ineligible at UPDATE (race with promotion / already uncached)",
                );
            }
            Err(e) => {
                return Err(CacheTickError::Demote {
                    dataset_id: candidate.id,
                    source: e,
                });
            }
        }
    }
    if skipped_races > 0 {
        tracing::info!(
            skipped_races,
            "cache tick: some demotions skipped to honour editorial pin / race",
        );
    }

    // 3. Compute the cache hit ratio. The Prometheus exporter
    //    (#2.10) will scrape this from the tracing emission until
    //    a real metrics handle exists.
    let ratio = state
        .cache_hit_ratio(config.hit_ratio_window_days)
        .await
        .map_err(CacheTickError::HitRatio)?;
    let ratio_value = ratio.ratio();
    tracing::info!(
        hits = ratio.hits,
        total = ratio.total,
        ratio = ?ratio_value,
        "cache hit ratio (last {} days)",
        config.hit_ratio_window_days,
    );

    Ok(CacheTickReport {
        hot_candidate_count: hot.len(),
        demoted_count: demoted,
        hit_ratio_hits: ratio.hits,
        hit_ratio_total: ratio.total,
    })
}

/// Per-tick config. Defaults match the Definition of Done selection rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheTickConfig {
    pub hot_window_days: i32,
    pub hot_hit_threshold: i64,
    pub demotion_inactive_days: i32,
    pub hit_ratio_window_days: i32,
}

impl Default for CacheTickConfig {
    fn default() -> Self {
        Self {
            hot_window_days: DEFAULT_HOT_WINDOW_DAYS,
            hot_hit_threshold: DEFAULT_HOT_HIT_THRESHOLD,
            demotion_inactive_days: DEFAULT_DEMOTION_INACTIVE_DAYS,
            hit_ratio_window_days: DEFAULT_HIT_RATIO_WINDOW_DAYS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;
    use storage::{CacheCandidate, CacheHitRatio};

    #[derive(Default)]
    struct MockState {
        hot: Mutex<Vec<CacheCandidate>>,
        cold: Mutex<Vec<CacheCandidate>>,
        hit_ratio: Mutex<CacheHitRatio>,
        demoted: Mutex<Vec<Uuid>>,
        demote_should_fail: Mutex<Option<Uuid>>,
    }

    #[async_trait]
    impl CacheState for MockState {
        async fn hot_candidates(
            &self,
            _window_days: i32,
            _hit_threshold: i64,
        ) -> Result<Vec<CacheCandidate>, StorageError> {
            Ok(self.hot.lock().unwrap().clone())
        }

        async fn cold_candidates(
            &self,
            _inactive_days: i32,
        ) -> Result<Vec<CacheCandidate>, StorageError> {
            Ok(self.cold.lock().unwrap().clone())
        }

        async fn demote_dataset(&self, dataset_id: Uuid) -> Result<bool, StorageError> {
            if let Some(fail_id) = *self.demote_should_fail.lock().unwrap() {
                if fail_id == dataset_id {
                    return Err(StorageError::InvalidArgument(
                        "test-injected failure".into(),
                    ));
                }
            }
            self.demoted.lock().unwrap().push(dataset_id);
            Ok(true)
        }

        async fn cache_hit_ratio(&self, _window_days: i32) -> Result<CacheHitRatio, StorageError> {
            Ok(*self.hit_ratio.lock().unwrap())
        }
    }

    fn candidate(id: u128, slug: &str, tier: &str, hits: i64) -> CacheCandidate {
        CacheCandidate {
            id: Uuid::from_u128(id),
            slug: slug.to_string(),
            tier: tier.to_string(),
            query_hits: hits,
        }
    }

    #[tokio::test]
    async fn empty_tick_reports_zero_counters() {
        let state = Arc::new(MockState::default());
        let report = run_cache_tick(state, CacheTickConfig::default())
            .await
            .unwrap();
        assert_eq!(report.hot_candidate_count, 0);
        assert_eq!(report.demoted_count, 0);
        assert_eq!(report.hit_ratio_hits, 0);
        assert_eq!(report.hit_ratio_total, 0);
        assert_eq!(report.hit_ratio(), None);
    }

    #[tokio::test]
    async fn promotion_candidates_logged_not_executed() {
        // v0.1: hot candidates only get logged. We assert by
        // counting them in the report; no demote should fire on
        // hot-candidate ids.
        let state = MockState::default();
        *state.hot.lock().unwrap() = vec![
            candidate(1, "tw-platinum-1", "platinum", 0),
            candidate(2, "tw-popular-1", "silver", 120),
        ];
        let state = Arc::new(state);
        let report = run_cache_tick(state.clone(), CacheTickConfig::default())
            .await
            .unwrap();
        assert_eq!(report.hot_candidate_count, 2);
        assert!(state.demoted.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn cold_candidates_get_demoted() {
        let state = MockState::default();
        *state.cold.lock().unwrap() = vec![
            candidate(10, "tw-stale-1", "bronze", 0),
            candidate(11, "tw-stale-2", "silver", 0),
        ];
        let state = Arc::new(state);
        let report = run_cache_tick(state.clone(), CacheTickConfig::default())
            .await
            .unwrap();
        assert_eq!(report.demoted_count, 2);
        let demoted = state.demoted.lock().unwrap();
        assert!(demoted.contains(&Uuid::from_u128(10)));
        assert!(demoted.contains(&Uuid::from_u128(11)));
    }

    /// R3 fix: `demote_dataset` returning `Ok(false)` means the
    /// row was no longer eligible at UPDATE time (race with a
    /// concurrent promotion). The tick must treat that as a
    /// skip — not count it as demoted — and continue with the
    /// remaining candidates.
    #[tokio::test]
    async fn demote_returning_false_is_skipped_not_counted() {
        #[derive(Default)]
        struct RaceMock {
            cold: Mutex<Vec<CacheCandidate>>,
            // Returning Ok(false) for the first id, Ok(true) for
            // the rest.
            race_id: Mutex<Option<Uuid>>,
            demoted: Mutex<Vec<Uuid>>,
        }
        #[async_trait]
        impl CacheState for RaceMock {
            async fn hot_candidates(
                &self,
                _w: i32,
                _t: i64,
            ) -> Result<Vec<CacheCandidate>, StorageError> {
                Ok(vec![])
            }
            async fn cold_candidates(&self, _d: i32) -> Result<Vec<CacheCandidate>, StorageError> {
                Ok(self.cold.lock().unwrap().clone())
            }
            async fn demote_dataset(&self, dataset_id: Uuid) -> Result<bool, StorageError> {
                if Some(dataset_id) == *self.race_id.lock().unwrap() {
                    return Ok(false);
                }
                self.demoted.lock().unwrap().push(dataset_id);
                Ok(true)
            }
            async fn cache_hit_ratio(&self, _w: i32) -> Result<CacheHitRatio, StorageError> {
                Ok(CacheHitRatio::default())
            }
        }
        let state = RaceMock::default();
        *state.cold.lock().unwrap() = vec![
            candidate(30, "tw-race-a", "bronze", 0),
            candidate(31, "tw-race-b", "bronze", 0),
        ];
        *state.race_id.lock().unwrap() = Some(Uuid::from_u128(30));
        let state = Arc::new(state);
        let report = run_cache_tick(state.clone(), CacheTickConfig::default())
            .await
            .unwrap();
        // Only the un-raced candidate was demoted.
        assert_eq!(report.demoted_count, 1);
        let demoted = state.demoted.lock().unwrap();
        assert_eq!(demoted.len(), 1);
        assert_eq!(demoted[0], Uuid::from_u128(31));
    }

    #[tokio::test]
    async fn demote_failure_aborts_tick() {
        let state = MockState::default();
        *state.cold.lock().unwrap() = vec![
            candidate(20, "tw-stale-a", "bronze", 0),
            candidate(21, "tw-stale-b", "bronze", 0),
        ];
        *state.demote_should_fail.lock().unwrap() = Some(Uuid::from_u128(20));
        let state = Arc::new(state);
        let err = run_cache_tick(state, CacheTickConfig::default())
            .await
            .expect_err("expected Demote error");
        match err {
            CacheTickError::Demote { dataset_id, .. } => {
                assert_eq!(dataset_id, Uuid::from_u128(20));
            }
            other => panic!("unexpected err: {other:?}"),
        }
    }

    #[tokio::test]
    async fn hit_ratio_passes_through() {
        let state = MockState::default();
        *state.hit_ratio.lock().unwrap() = CacheHitRatio {
            hits: 75,
            total: 100,
        };
        let state = Arc::new(state);
        let report = run_cache_tick(state, CacheTickConfig::default())
            .await
            .unwrap();
        assert_eq!(report.hit_ratio_hits, 75);
        assert_eq!(report.hit_ratio_total, 100);
        assert!((report.hit_ratio().unwrap() - 0.75).abs() < 1e-9);
    }

    #[test]
    fn defaults_match_dod() {
        let cfg = CacheTickConfig::default();
        assert_eq!(cfg.hot_window_days, 7);
        assert_eq!(cfg.hot_hit_threshold, 50);
        assert_eq!(cfg.demotion_inactive_days, 30);
    }
}
