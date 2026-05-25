//! Nightly tier classifier (#2.8) — popularity + authority + format.
//!
//! Composes a three-step pipeline:
//!
//! 1. Snapshot every dataset's scoring inputs via
//!    [`storage::TierRepo::list_for_tiering`].
//! 2. Score each row with the pure [`compute_score`] against a
//!    [`TierConfig`] loaded once at boot from `config/tiers.toml`.
//! 3. Persist the result via
//!    [`storage::TierRepo::apply_computed_tiers`], which honours
//!    `datasets.tier_override` unconditionally — curators always
//!    have the final word.
//!
//! The scorer is deliberately a free function: tests can drive it
//! without touching Postgres, and the per-call cost stays at
//! "iterate three small lookups" so the nightly tick is bounded
//! by SQL round-trips, not Rust compute.
//!
//! Formula (matches `docs/DESIGN.md §4.5`):
//!
//! ```text
//! score = w.update_frequency   · update_frequency_scores[ds.update_frequency]
//!       + w.publisher_authority · best publisher_authority_rules match
//!       + w.format_quality     · format_scores[ds.format]
//!       + w.access_count       · clamp01(access_count / access.normalize_cap)
//!
//! tier = first { platinum, gold, silver } whose threshold the
//!        score strictly exceeds; else bronze.
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;
use storage::{ComputedTier, StorageError, TierDatasetRow, TierRepo};
use thiserror::Error;

/// String labels mirror the `datasets.tier` CHECK constraint
/// values. Exposed as constants so callers can compare without
/// stringly-typed magic literals.
pub const TIER_PLATINUM: &str = "platinum";
pub const TIER_GOLD: &str = "gold";
pub const TIER_SILVER: &str = "silver";
pub const TIER_BRONZE: &str = "bronze";

/// How close the four `weights` are allowed to drift from 1.0
/// before [`parse_config`] refuses to load. Generous enough to
/// absorb the usual decimal-printf round-trip; tight enough to
/// catch a hand-edit where one row was bumped without rebalancing
/// the rest.
const WEIGHT_SUM_EPSILON: f64 = 1e-6;

// ── Config (Deserialised verbatim from config/tiers.toml) ────────────

/// Top-level shape of `config/tiers.toml`. All fields are required
/// — defaults would let a partially-edited file silently degrade
/// classification without anyone noticing.
///
/// `publisher_authority` is its own table (rather than two
/// sibling keys) because TOML attributes any naked key that
/// follows a `[section]` header to that section. A flat
/// `publisher_authority_default` would silently land inside
/// whatever `[…_scores]` table happened to precede it; nesting
/// makes the binding unambiguous.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct TierConfig {
    pub weights: TierWeights,
    pub thresholds: TierThresholds,
    pub update_frequency_scores: BTreeMap<String, f64>,
    pub format_scores: BTreeMap<String, f64>,
    pub publisher_authority: PublisherAuthorityConfig,
    pub access: AccessConfig,
}

/// Publisher-authority lookup: a fallback default plus an ordered
/// list of substring rules. First matching rule wins; falls
/// through to `default` when no rule matches.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PublisherAuthorityConfig {
    pub default: f64,
    #[serde(default)]
    pub rules: Vec<PublisherAuthorityRule>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
pub struct TierWeights {
    pub update_frequency: f64,
    pub publisher_authority: f64,
    pub format_quality: f64,
    pub access_count: f64,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
pub struct TierThresholds {
    pub platinum: f64,
    pub gold: f64,
    pub silver: f64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PublisherAuthorityRule {
    /// Substring matched against `datasets.publisher`. Case-
    /// sensitive (publisher names are zh-TW; ASCII rules would
    /// add no signal at the cost of mojibake surprises).
    #[serde(rename = "match")]
    pub matcher: String,
    pub score: f64,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
pub struct AccessConfig {
    /// Lookback window for `usage_records`, in days.
    pub window_days: u32,
    /// Hit count at which the access-count signal saturates at
    /// 1.0. Counts above the cap don't add more weight — the
    /// other dimensions still need room to influence the final
    /// tier.
    pub normalize_cap: u32,
}

#[derive(Debug, Error)]
pub enum TierConfigError {
    #[error("could not read tier config {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("could not parse tier config {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("tier config {path}: {message}")]
    Invalid { path: String, message: String },
}

/// Load + validate `config/tiers.toml`. Used by `main.rs` at boot;
/// the worker panics on a `TierConfigError` so a typoed config
/// fails fast.
pub fn load_config<P: AsRef<Path>>(path: P) -> Result<TierConfig, TierConfigError> {
    let path_str = path.as_ref().display().to_string();
    let raw = fs::read_to_string(&path).map_err(|e| TierConfigError::Read {
        path: path_str.clone(),
        source: e,
    })?;
    parse_config(&raw, &path_str)
}

/// Parse already-read TOML text. Split from [`load_config`] so
/// tests can drive the validator without a tempfile.
pub fn parse_config(raw: &str, label: &str) -> Result<TierConfig, TierConfigError> {
    let cfg: TierConfig = toml::from_str(raw).map_err(|e| TierConfigError::Parse {
        path: label.to_string(),
        source: e,
    })?;
    validate(&cfg, label)?;
    Ok(cfg)
}

fn validate(cfg: &TierConfig, label: &str) -> Result<(), TierConfigError> {
    let w = &cfg.weights;
    let sum = w.update_frequency + w.publisher_authority + w.format_quality + w.access_count;
    if (sum - 1.0).abs() > WEIGHT_SUM_EPSILON {
        return Err(TierConfigError::Invalid {
            path: label.to_string(),
            message: format!("weights must sum to 1.0 (within {WEIGHT_SUM_EPSILON}); got {sum}"),
        });
    }
    for (field, value) in [
        ("weights.update_frequency", w.update_frequency),
        ("weights.publisher_authority", w.publisher_authority),
        ("weights.format_quality", w.format_quality),
        ("weights.access_count", w.access_count),
    ] {
        if !(0.0..=1.0).contains(&value) {
            return Err(TierConfigError::Invalid {
                path: label.to_string(),
                message: format!("{field} must be in [0.0, 1.0]; got {value}"),
            });
        }
    }
    let t = &cfg.thresholds;
    if !(t.platinum > t.gold && t.gold > t.silver && t.silver >= 0.0 && t.platinum <= 1.0) {
        return Err(TierConfigError::Invalid {
            path: label.to_string(),
            message: format!(
                "thresholds must satisfy 1.0 ≥ platinum > gold > silver ≥ 0.0; got \
                 platinum={p}, gold={g}, silver={s}",
                p = t.platinum,
                g = t.gold,
                s = t.silver,
            ),
        });
    }
    if cfg.access.normalize_cap == 0 {
        return Err(TierConfigError::Invalid {
            path: label.to_string(),
            message: "access.normalize_cap must be > 0 (it's the saturation denominator)".into(),
        });
    }
    Ok(())
}

// ── Scoring (pure) ────────────────────────────────────────────────────

/// Pick the tier label that matches a raw score. Walks the
/// thresholds descending so equal-precision values land at the
/// boundary they're authored against.
#[must_use]
pub fn classify(score: f64, thresholds: &TierThresholds) -> &'static str {
    if score > thresholds.platinum {
        TIER_PLATINUM
    } else if score > thresholds.gold {
        TIER_GOLD
    } else if score > thresholds.silver {
        TIER_SILVER
    } else {
        TIER_BRONZE
    }
}

/// Best matching publisher-authority score, or the default when
/// no rule matches. Rules are walked in the order
/// `config/tiers.toml` declared them — earlier entries win.
fn publisher_authority_score(publisher: Option<&str>, cfg: &TierConfig) -> f64 {
    let Some(p) = publisher else {
        return 0.0;
    };
    for rule in &cfg.publisher_authority.rules {
        if !rule.matcher.is_empty() && p.contains(&rule.matcher) {
            return rule.score;
        }
    }
    cfg.publisher_authority.default
}

/// Compute the final `(tier, score)` for one dataset. Pure
/// function — no I/O, no allocation beyond the lookup borrows.
/// Score is always finite and in `[0.0, 1.0]` provided the config
/// `weights` and lookup values are in range (enforced at load).
#[must_use]
pub fn compute_score(input: &TierDatasetRow, cfg: &TierConfig) -> ComputedTier {
    let update_signal = input
        .update_frequency
        .as_deref()
        .and_then(|k| cfg.update_frequency_scores.get(k).copied())
        .unwrap_or(0.0);

    let format_signal = input
        .format
        .as_deref()
        .and_then(|k| cfg.format_scores.get(k).copied())
        .unwrap_or(0.0);

    let authority_signal = publisher_authority_score(input.publisher.as_deref(), cfg);

    // Saturating ratio: counts above the cap pin to 1.0; a zero cap
    // is rejected at config load so the divide is safe.
    let cap = f64::from(cfg.access.normalize_cap);
    let count = input.access_count.max(0);
    #[allow(clippy::cast_precision_loss)]
    let count_f = count as f64;
    let access_signal = (count_f / cap).min(1.0);

    let w = cfg.weights;
    let score = w.update_frequency * update_signal
        + w.publisher_authority * authority_signal
        + w.format_quality * format_signal
        + w.access_count * access_signal;

    ComputedTier {
        id: input.id,
        tier: classify(score, &cfg.thresholds).to_owned(),
        score,
    }
}

// ── Tick orchestration ────────────────────────────────────────────────

/// Summary the nightly job emits in its structured log line. Counts
/// make the bucket distribution observable at a glance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TierTickReport {
    pub scanned: usize,
    pub platinum: usize,
    pub gold: usize,
    pub silver: usize,
    pub bronze: usize,
    pub updated: u64,
}

/// Run one nightly tick: read inputs, score every row in memory,
/// persist via the repo. The repo enforces the `tier_override`
/// skip — this function never touches the override directly.
pub async fn run_tier_tick<R: TierRepo + ?Sized>(
    repo: &R,
    config: &TierConfig,
) -> Result<TierTickReport, StorageError> {
    #[allow(clippy::cast_possible_wrap)]
    let window_days = config.access.window_days.min(i32::MAX as u32) as i32;
    let inputs = repo.list_for_tiering(window_days).await?;
    let mut report = TierTickReport {
        scanned: inputs.len(),
        ..TierTickReport::default()
    };
    let mut computed = Vec::with_capacity(inputs.len());
    for row in &inputs {
        let ct = compute_score(row, config);
        match ct.tier.as_str() {
            TIER_PLATINUM => report.platinum += 1,
            TIER_GOLD => report.gold += 1,
            TIER_SILVER => report.silver += 1,
            _ => report.bronze += 1,
        }
        computed.push(ct);
    }
    report.updated = repo.apply_computed_tiers(&computed).await?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;
    use uuid::Uuid;

    /// Default config used across the scorer tests. Mirrors the
    /// shipped `config/tiers.toml` values so test assertions match
    /// production behavior; tests that need a different weighting
    /// override one field at a time.
    fn default_config() -> TierConfig {
        TierConfig {
            weights: TierWeights {
                update_frequency: 0.4,
                publisher_authority: 0.3,
                format_quality: 0.2,
                access_count: 0.1,
            },
            thresholds: TierThresholds {
                platinum: 0.85,
                gold: 0.70,
                silver: 0.50,
            },
            update_frequency_scores: BTreeMap::from([
                ("daily".to_owned(), 1.0),
                ("weekly".to_owned(), 0.8),
                ("monthly".to_owned(), 0.6),
                ("quarterly".to_owned(), 0.4),
                ("yearly".to_owned(), 0.2),
            ]),
            format_scores: BTreeMap::from([
                ("parquet".to_owned(), 1.0),
                ("csv".to_owned(), 1.0),
                ("json".to_owned(), 1.0),
                ("geojson".to_owned(), 0.9),
                ("xml".to_owned(), 0.7),
                ("xlsx".to_owned(), 0.6),
                ("pdf".to_owned(), 0.2),
            ]),
            publisher_authority: PublisherAuthorityConfig {
                default: 0.4,
                rules: vec![
                    PublisherAuthorityRule {
                        matcher: "行政院".to_owned(),
                        score: 1.0,
                    },
                    PublisherAuthorityRule {
                        matcher: "部".to_owned(),
                        score: 0.9,
                    },
                    PublisherAuthorityRule {
                        matcher: "署".to_owned(),
                        score: 0.9,
                    },
                    PublisherAuthorityRule {
                        matcher: "政府".to_owned(),
                        score: 0.6,
                    },
                ],
            },
            access: AccessConfig {
                window_days: 7,
                normalize_cap: 50,
            },
        }
    }

    fn input(
        publisher: Option<&str>,
        update_frequency: Option<&str>,
        format: Option<&str>,
        access_count: i64,
    ) -> TierDatasetRow {
        TierDatasetRow {
            id: Uuid::nil(),
            publisher: publisher.map(str::to_owned),
            update_frequency: update_frequency.map(str::to_owned),
            format: format.map(str::to_owned),
            access_count,
        }
    }

    #[test]
    fn classify_walks_thresholds_descending() {
        let t = TierThresholds {
            platinum: 0.85,
            gold: 0.70,
            silver: 0.50,
        };
        assert_eq!(classify(0.86, &t), TIER_PLATINUM);
        assert_eq!(classify(0.85, &t), TIER_GOLD); // NOT platinum — strict >
        assert_eq!(classify(0.71, &t), TIER_GOLD);
        assert_eq!(classify(0.70, &t), TIER_SILVER); // boundary → next-down
        assert_eq!(classify(0.51, &t), TIER_SILVER);
        assert_eq!(classify(0.50, &t), TIER_BRONZE);
        assert_eq!(classify(0.0, &t), TIER_BRONZE);
        // Out-of-range inputs land sensibly.
        assert_eq!(classify(1.0, &t), TIER_PLATINUM);
        assert_eq!(classify(-0.1, &t), TIER_BRONZE);
    }

    #[test]
    fn compute_score_uses_all_four_dimensions() {
        let cfg = default_config();
        // Every dimension at max → 0.4·1 + 0.3·1 + 0.2·1 + 0.1·1 = 1.0
        let row = input(Some("行政院"), Some("daily"), Some("csv"), 60);
        let out = compute_score(&row, &cfg);
        assert!((out.score - 1.0).abs() < 1e-9, "got {}", out.score);
        assert_eq!(out.tier, TIER_PLATINUM);
    }

    #[test]
    fn compute_score_treats_missing_lookups_as_zero_signal() {
        let cfg = default_config();
        // unknown publisher → fallback default (0.4); unknown
        // update_frequency / format → 0.0; no access.
        let row = input(
            Some("Acme Corp"),
            Some("hourly"), // not in the table
            Some("pdf-archive"),
            0,
        );
        let out = compute_score(&row, &cfg);
        // 0.3 · 0.4 = 0.12 → bronze
        assert!((out.score - 0.12).abs() < 1e-9, "got {}", out.score);
        assert_eq!(out.tier, TIER_BRONZE);
    }

    #[test]
    fn compute_score_handles_null_publisher_format_frequency() {
        let cfg = default_config();
        let row = input(None, None, None, 0);
        let out = compute_score(&row, &cfg);
        assert!((out.score - 0.0).abs() < 1e-9);
        assert_eq!(out.tier, TIER_BRONZE);
    }

    #[test]
    fn compute_score_access_saturates_at_cap() {
        let cfg = default_config();
        // Hold everything else constant, vary access count past the
        // 50-row cap. At 100 hits the access signal must still be
        // 1.0; the dimension can't outvote others by overshooting.
        let base = input(Some("部"), Some("monthly"), Some("csv"), 0);
        let mut sat = base.clone();
        sat.access_count = 1_000_000;
        let s_base = compute_score(&base, &cfg);
        let s_sat = compute_score(&sat, &cfg);
        // base: 0.4·0.6 + 0.3·0.9 + 0.2·1 + 0.1·0 = 0.71
        // sat:  0.4·0.6 + 0.3·0.9 + 0.2·1 + 0.1·1 = 0.81
        assert!((s_base.score - 0.71).abs() < 1e-9);
        assert!((s_sat.score - 0.81).abs() < 1e-9);
        // Beyond the cap, signal is pinned — a 10× counter doesn't
        // shift score further.
        let mut over = sat.clone();
        over.access_count = 10_000_000;
        let s_over = compute_score(&over, &cfg);
        assert!((s_over.score - s_sat.score).abs() < 1e-9);
    }

    #[test]
    fn publisher_rules_first_match_wins() {
        let cfg = default_config();
        // "行政院主計總處" contains BOTH "行政院" (rule 1, 1.0) and
        // "處" (no rule but matches a future "局/處" rule). Rule 1
        // wins because it's declared first.
        assert!((publisher_authority_score(Some("行政院主計總處"), &cfg) - 1.0).abs() < 1e-9);
        // "交通部觀光署" contains BOTH "部" (rule 2) and "署" (rule 3)
        // — both score 0.9 so order doesn't matter, but the test
        // pins behavior.
        assert!((publisher_authority_score(Some("交通部觀光署"), &cfg) - 0.9).abs() < 1e-9);
        // No rule matches → default.
        assert!((publisher_authority_score(Some("Acme Ltd"), &cfg) - 0.4).abs() < 1e-9);
        // No publisher → 0.0 (not the default — the dimension has
        // no input to score).
        assert!((publisher_authority_score(None, &cfg) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn publisher_rules_skip_empty_matcher() {
        let mut cfg = default_config();
        // An accidentally-empty matcher would substring-match every
        // input (`.contains("")` is always true), silently
        // collapsing every publisher to the first rule's score.
        // The scorer must skip empty matchers so the file can be
        // hand-edited without a footgun.
        cfg.publisher_authority.rules.insert(
            0,
            PublisherAuthorityRule {
                matcher: String::new(),
                score: 0.0,
            },
        );
        // "行政院" still scores 1.0 via the rule that follows the
        // empty one.
        assert!((publisher_authority_score(Some("行政院"), &cfg) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn parse_config_round_trips_shipped_defaults() {
        // The committed `config/tiers.toml` must round-trip through
        // the validator — a CI-side guarantee that the shipped file
        // never silently degrades.
        let raw = include_str!("../../../config/tiers.toml");
        let cfg = parse_config(raw, "config/tiers.toml").expect("shipped config must parse");
        let sum = cfg.weights.update_frequency
            + cfg.weights.publisher_authority
            + cfg.weights.format_quality
            + cfg.weights.access_count;
        assert!((sum - 1.0).abs() < WEIGHT_SUM_EPSILON);
        assert!(cfg.thresholds.platinum > cfg.thresholds.gold);
        assert!(cfg.thresholds.gold > cfg.thresholds.silver);
    }

    #[test]
    fn parse_config_rejects_weight_sum_drift() {
        // Bump update_frequency to 0.5 (sum = 1.1) — should reject.
        let bad = r"
[weights]
update_frequency = 0.5
publisher_authority = 0.3
format_quality = 0.2
access_count = 0.1

[thresholds]
platinum = 0.85
gold = 0.7
silver = 0.5

[update_frequency_scores]
daily = 1.0

[format_scores]
csv = 1.0

[publisher_authority]
default = 0.4
rules = []

[access]
window_days = 7
normalize_cap = 50
";
        let err = parse_config(bad, "test").unwrap_err();
        assert!(
            matches!(err, TierConfigError::Invalid { ref message, .. } if message.contains("sum to 1.0")),
            "got: {err}",
        );
    }

    #[test]
    fn parse_config_rejects_misordered_thresholds() {
        let bad = r"
[weights]
update_frequency = 0.4
publisher_authority = 0.3
format_quality = 0.2
access_count = 0.1

[thresholds]
platinum = 0.5
gold = 0.7
silver = 0.85

[update_frequency_scores]
daily = 1.0

[format_scores]
csv = 1.0

[publisher_authority]
default = 0.4
rules = []

[access]
window_days = 7
normalize_cap = 50
";
        let err = parse_config(bad, "test").unwrap_err();
        assert!(
            matches!(err, TierConfigError::Invalid { ref message, .. } if message.contains("threshold")),
            "got: {err}",
        );
    }

    #[test]
    fn parse_config_rejects_zero_normalize_cap() {
        let bad = r"
[weights]
update_frequency = 0.4
publisher_authority = 0.3
format_quality = 0.2
access_count = 0.1

[thresholds]
platinum = 0.85
gold = 0.7
silver = 0.5

[update_frequency_scores]
daily = 1.0

[format_scores]
csv = 1.0

[publisher_authority]
default = 0.4
rules = []

[access]
window_days = 7
normalize_cap = 0
";
        let err = parse_config(bad, "test").unwrap_err();
        assert!(
            matches!(err, TierConfigError::Invalid { ref message, .. } if message.contains("normalize_cap")),
            "got: {err}",
        );
    }

    /// In-memory `TierRepo` stub so the tick orchestrator can be
    /// exercised end-to-end without Postgres. Captures the
    /// computed batch so tests can assert the orchestrator passed
    /// every row through.
    struct StubRepo {
        rows: Vec<TierDatasetRow>,
        captured: Mutex<Vec<ComputedTier>>,
        apply_outcome: u64,
    }

    impl StubRepo {
        fn new(rows: Vec<TierDatasetRow>, apply_outcome: u64) -> Self {
            Self {
                rows,
                captured: Mutex::new(Vec::new()),
                apply_outcome,
            }
        }
    }

    #[async_trait]
    impl TierRepo for StubRepo {
        async fn list_for_tiering(
            &self,
            _window_days: i32,
        ) -> Result<Vec<TierDatasetRow>, StorageError> {
            Ok(self.rows.clone())
        }

        async fn apply_computed_tiers(
            &self,
            computed: &[ComputedTier],
        ) -> Result<u64, StorageError> {
            self.captured.lock().unwrap().extend_from_slice(computed);
            Ok(self.apply_outcome)
        }
    }

    #[tokio::test]
    async fn run_tier_tick_buckets_and_reports() {
        // Three datasets engineered to land in three different
        // buckets given the default config.
        let rows = vec![
            // platinum: every dimension max
            TierDatasetRow {
                id: Uuid::from_u128(1),
                publisher: Some("行政院".into()),
                update_frequency: Some("daily".into()),
                format: Some("csv".into()),
                access_count: 50,
            },
            // silver: monthly · 部 · xlsx · no access
            // 0.4·0.6 + 0.3·0.9 + 0.2·0.6 + 0 = 0.63 → silver
            TierDatasetRow {
                id: Uuid::from_u128(2),
                publisher: Some("教育部".into()),
                update_frequency: Some("monthly".into()),
                format: Some("xlsx".into()),
                access_count: 0,
            },
            // bronze: null everything
            TierDatasetRow {
                id: Uuid::from_u128(3),
                publisher: None,
                update_frequency: None,
                format: None,
                access_count: 0,
            },
        ];
        let repo = StubRepo::new(rows, 3);
        let cfg = default_config();
        let report = run_tier_tick(&repo, &cfg).await.expect("tick succeeds");
        assert_eq!(report.scanned, 3);
        assert_eq!(report.platinum, 1);
        assert_eq!(report.silver, 1);
        assert_eq!(report.bronze, 1);
        assert_eq!(report.gold, 0);
        assert_eq!(report.updated, 3);
        // The orchestrator pushed all three computed tiers through.
        let captured = repo.captured.lock().unwrap();
        assert_eq!(captured.len(), 3);
        assert_eq!(captured[0].tier, TIER_PLATINUM);
        assert_eq!(captured[1].tier, TIER_SILVER);
        assert_eq!(captured[2].tier, TIER_BRONZE);
    }

    #[tokio::test]
    async fn run_tier_tick_empty_catalog_is_no_op() {
        let repo = StubRepo::new(vec![], 0);
        let cfg = default_config();
        let report = run_tier_tick(&repo, &cfg).await.expect("tick succeeds");
        assert_eq!(report, TierTickReport::default());
    }
}
