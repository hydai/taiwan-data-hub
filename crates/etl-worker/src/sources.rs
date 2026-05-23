//! `config/sources.toml` loader (#5b.1).
//!
//! Reads the per-source registry that drives cron
//! scheduling. The TOML structure is intentionally flat —
//! one section per source — so an operator can disable a
//! flaky upstream by toggling `enabled = false` and
//! restarting the worker. Adding a new source is a config
//! edit + a connector impl + a `SourceId` enum row
//! (matched against `datasets.source` at the SQL layer).
//!
//! The loader keeps the wire shape (`SourceFileConfig`)
//! separate from the runtime shape (`SourceConfig`) so
//! the runtime shape can carry pre-validated `Duration`s
//! and an enum `SourceId` instead of strings. Validation
//! happens once at boot; the cron-job closures see
//! already-parsed values.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

use connectors::SourceId;
use serde::Deserialize;
use thiserror::Error;

use crate::retry::RetryConfig;

/// Upper bound on `retry_max_attempts`. Past this the
/// retry envelope produces a budget that's silly under
/// any reasonable interpretation (a single crawl that
/// takes longer than the next cron tick is dead work).
/// Picked at 1000 so the bound is generous enough not to
/// surprise operators tuning aggressive sources, but
/// keeps the DLQ `attempts INTEGER` column safely within
/// `i32::MAX` (≈ 2.1 billion).
const MAX_RETRY_ATTEMPTS: u32 = 1000;

/// Upper bound on `retry_*_backoff_secs`. 24 hours is
/// generous enough for daily-cron sources to sleep an
/// entire cron tick between retries if an operator
/// really wants to, but bounds the `Duration` math in
/// the retry envelope so `checked_mul(2)` can't overflow
/// in any realistic configuration. Without this cap a
/// hand-written value like `u64::MAX` would trip the
/// envelope's saturating fallback on every iteration —
/// safe, but a misconfig that should fail boot.
const MAX_BACKOFF_SECS: u64 = 86_400;

/// Wire shape — directly deserialised from `sources.toml`.
/// Strings are kept as-is; conversion to the runtime
/// shape (`SourceConfig`) does enum + duration parsing.
#[derive(Debug, Clone, Deserialize)]
struct SourceFileConfig {
    sources: BTreeMap<String, SourceEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct SourceEntry {
    enabled: bool,
    cron_utc: String,
    retry_max_attempts: u32,
    retry_initial_backoff_secs: u64,
    retry_max_backoff_secs: u64,
}

/// Runtime shape — one per enabled source. Carries the
/// already-parsed `SourceId` enum + a typed `RetryConfig`.
///
/// `cron_utc` is owned (not `&'static str`) so the
/// loader doesn't leak a heap allocation per source.
/// `tokio-cron-scheduler`'s `Job::new_async_tz` takes
/// the expression by `&str`, so the owned `String` is
/// passed through with a borrow at the call site. The
/// struct is `Clone` (not `Copy`) because of the
/// owned string field; call sites that need to thread
/// the config into a closure clone explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceConfig {
    pub source_id: SourceId,
    /// 7-field UTC cron expression passed to
    /// `Job::new_async_tz`.
    pub cron_utc: String,
    pub retry: RetryConfig,
}

/// Boot-time loader errors. All variants are fatal —
/// the worker refuses to start with a malformed config
/// rather than skip the offending source silently.
#[derive(Debug, Error)]
pub enum SourceConfigError {
    #[error("could not read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("could not parse {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("unknown source {name:?} — not a valid datasets.source value")]
    UnknownSource { name: String },
    /// `user_contrib` is a valid `datasets.source` value
    /// (community-submitted datasets land there) but
    /// isn't ETL-driven — it has no upstream crawl.
    /// Separate variant from `UnknownSource` so the
    /// operator-facing error message can be accurate:
    /// "this name is real, but it doesn't belong here"
    /// is a different fix from "this name is a typo".
    /// `source_path` carries the actual loaded config
    /// path so the message names the file the operator
    /// is editing (defaults to `config/sources.toml`,
    /// overridable via `SOURCES_CONFIG_PATH`).
    #[error(
        "source {name:?} is a valid datasets.source value but is not ETL-driven; remove it from {source_path}"
    )]
    NotEtlDriven { name: String, source_path: String },
    #[error("source {name:?}: retry_max_attempts must be between 1 and {limit} (got {value})")]
    InvalidAttempts {
        name: String,
        value: u32,
        limit: u32,
    },
    #[error(
        "source {name:?}: retry_initial_backoff_secs ({initial}) must be ≤ retry_max_backoff_secs ({max})"
    )]
    BackoffOutOfOrder {
        name: String,
        initial: u64,
        max: u64,
    },
    #[error(
        "source {name:?}: backoff seconds out of range — values must be ≤ {limit} (got initial={initial}, max={max})"
    )]
    BackoffTooLarge {
        name: String,
        initial: u64,
        max: u64,
        limit: u64,
    },
    /// Zero backoff combined with retries-enabled is a
    /// tight loop that hammers upstream. The loader
    /// rejects it. If a deploy genuinely wants no-delay
    /// retries (e.g., for tests), it must set
    /// `retry_max_attempts = 1` so the envelope is a no-op
    /// path through.
    #[error(
        "source {name:?}: zero backoff is only allowed when retry_max_attempts = 1 (got max_attempts={attempts}, initial={initial})"
    )]
    ZeroBackoffWithRetries {
        name: String,
        attempts: u32,
        initial: u64,
    },
}

/// Read `config/sources.toml` and return one
/// [`SourceConfig`] per enabled source.
///
/// **Disabled sources** (`enabled = false`) skip semantic
/// **validation** — their cron expression, retry attempts,
/// and backoff bounds aren't checked. They still have to
/// PARSE structurally though: the four required fields
/// (`cron_utc`, `retry_max_attempts`,
/// `retry_initial_backoff_secs`, `retry_max_backoff_secs`)
/// must be present and correctly typed. That's a TOML
/// deserialisation requirement, not a loader policy.
/// Operators leave a fully-populated disabled row as
/// documentation for the planned schedule a future PR
/// will turn on — see the M5b.2–5 placeholder rows in
/// `config/sources.toml`.
///
/// `cron_utc` strings are NOT validated here even for
/// enabled sources — the cron scheduler is the authority
/// on expression syntax, and it surfaces a clear error
/// at registration time. Re-validating in two places
/// invites drift.
pub fn load<P: AsRef<Path>>(path: P) -> Result<Vec<SourceConfig>, SourceConfigError> {
    let path_str = path.as_ref().display().to_string();
    let raw = fs::read_to_string(&path).map_err(|e| SourceConfigError::Read {
        path: path_str.clone(),
        source: e,
    })?;
    parse(&raw, &path_str)
}

/// Parse + validate already-read TOML text. Split from
/// [`load`] so tests don't need a temp file.
pub fn parse(raw: &str, label: &str) -> Result<Vec<SourceConfig>, SourceConfigError> {
    let file: SourceFileConfig = toml::from_str(raw).map_err(|e| SourceConfigError::Parse {
        path: label.to_string(),
        source: e,
    })?;
    let mut out = Vec::new();
    for (name, entry) in file.sources {
        if !entry.enabled {
            continue;
        }
        let source_id = match source_id_from_name(&name) {
            ParsedSource::Known(id) => id,
            ParsedSource::NotEtlDriven => {
                return Err(SourceConfigError::NotEtlDriven {
                    name,
                    source_path: label.to_string(),
                });
            }
            ParsedSource::Unknown => {
                return Err(SourceConfigError::UnknownSource { name });
            }
        };
        if entry.retry_max_attempts < 1 || entry.retry_max_attempts > MAX_RETRY_ATTEMPTS {
            return Err(SourceConfigError::InvalidAttempts {
                name,
                value: entry.retry_max_attempts,
                limit: MAX_RETRY_ATTEMPTS,
            });
        }
        if entry.retry_initial_backoff_secs > entry.retry_max_backoff_secs {
            return Err(SourceConfigError::BackoffOutOfOrder {
                name,
                initial: entry.retry_initial_backoff_secs,
                max: entry.retry_max_backoff_secs,
            });
        }
        // Zero initial backoff + multiple attempts is a
        // tight retry loop — hammers the upstream and
        // floods the log stream. Only allow zero when
        // retries are explicitly disabled (max_attempts =
        // 1). See `ZeroBackoffWithRetries` for the
        // operator-facing message.
        if entry.retry_max_attempts > 1 && entry.retry_initial_backoff_secs == 0 {
            return Err(SourceConfigError::ZeroBackoffWithRetries {
                name,
                attempts: entry.retry_max_attempts,
                initial: entry.retry_initial_backoff_secs,
            });
        }
        // Bound the Duration math in the retry envelope:
        // `checked_mul(2)` saturates on overflow, but
        // catching the misconfig at boot is louder than
        // running a worker whose backoffs always clamp.
        if entry.retry_initial_backoff_secs > MAX_BACKOFF_SECS
            || entry.retry_max_backoff_secs > MAX_BACKOFF_SECS
        {
            return Err(SourceConfigError::BackoffTooLarge {
                name,
                initial: entry.retry_initial_backoff_secs,
                max: entry.retry_max_backoff_secs,
                limit: MAX_BACKOFF_SECS,
            });
        }
        out.push(SourceConfig {
            source_id,
            cron_utc: entry.cron_utc,
            retry: RetryConfig {
                max_attempts: entry.retry_max_attempts,
                initial_backoff: Duration::from_secs(entry.retry_initial_backoff_secs),
                max_backoff: Duration::from_secs(entry.retry_max_backoff_secs),
            },
        });
    }
    Ok(out)
}

/// Three-state classifier for a `sources.toml` section
/// name. Splits "known and ETL-driven" (the happy path)
/// from "known but not ETL-driven" (`user_contrib`) from
/// "unknown" (typos / new sources not yet in
/// `datasets.source`).
enum ParsedSource {
    Known(SourceId),
    /// A valid `datasets.source` value that doesn't have
    /// a connector (today: `user_contrib`).
    NotEtlDriven,
    /// Not in `datasets.source` at all.
    Unknown,
}

/// `sources.toml` section name → classified result. The
/// names must match `datasets.source`'s SQL CHECK enum
/// exactly: `data_gov_tw`, `twse`, `moea`, `cwa`,
/// `fishery_moa`, `user_contrib`.
fn source_id_from_name(name: &str) -> ParsedSource {
    match name {
        "data_gov_tw" => ParsedSource::Known(SourceId::DataGovTw),
        "twse" => ParsedSource::Known(SourceId::Twse),
        "moea" => ParsedSource::Known(SourceId::Moea),
        "cwa" => ParsedSource::Known(SourceId::Cwa),
        "fishery_moa" => ParsedSource::Known(SourceId::FisheryMoa),
        "user_contrib" => ParsedSource::NotEtlDriven,
        _ => ParsedSource::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
[sources.data_gov_tw]
enabled = true
cron_utc = "0 0 18 * * * *"
retry_max_attempts = 3
retry_initial_backoff_secs = 30
retry_max_backoff_secs = 1800

[sources.twse]
enabled = false
cron_utc = "0 0 22 * * * *"
retry_max_attempts = 3
retry_initial_backoff_secs = 30
retry_max_backoff_secs = 1800
"#;

    #[test]
    fn happy_path_only_enabled_sources_return() {
        let configs = parse(VALID, "test").expect("parse valid");
        assert_eq!(configs.len(), 1);
        let c = &configs[0];
        assert_eq!(c.source_id, SourceId::DataGovTw);
        assert_eq!(c.cron_utc, "0 0 18 * * * *");
        assert_eq!(c.retry.max_attempts, 3);
        assert_eq!(c.retry.initial_backoff, Duration::from_secs(30));
        assert_eq!(c.retry.max_backoff, Duration::from_secs(1800));
    }

    #[test]
    fn unknown_source_rejected_at_boot() {
        let raw = r#"
[sources.bogus]
enabled = true
cron_utc = "0 0 0 * * * *"
retry_max_attempts = 1
retry_initial_backoff_secs = 1
retry_max_backoff_secs = 1
"#;
        let err = parse(raw, "test").unwrap_err();
        assert!(matches!(
            err,
            SourceConfigError::UnknownSource { name } if name == "bogus"
        ));
    }

    /// `user_contrib` is a valid `datasets.source` value
    /// but isn't ETL-driven. Loader must reject it with
    /// the dedicated `NotEtlDriven` variant so the boot-
    /// time error message points the operator at the
    /// right fix ("remove from sources.toml") rather
    /// than the wrong one ("fix the typo").
    #[test]
    fn user_contrib_rejected_as_not_etl_driven() {
        let raw = r#"
[sources.user_contrib]
enabled = true
cron_utc = "0 0 0 * * * *"
retry_max_attempts = 1
retry_initial_backoff_secs = 1
retry_max_backoff_secs = 1
"#;
        let err = parse(raw, "test").unwrap_err();
        assert!(matches!(
            &err,
            SourceConfigError::NotEtlDriven { name, source_path }
                if name == "user_contrib" && source_path == "test"
        ));
    }

    #[test]
    fn not_etl_driven_error_message_carries_custom_path() {
        // Same operator-UX pattern as
        // `build_connector_error_message_carries_custom_path`
        // on the main.rs side: when the loader runs with a
        // non-default config path (via SOURCES_CONFIG_PATH),
        // the rejection message must name the actual file
        // so the operator edits the right one.
        let raw = r#"
[sources.user_contrib]
enabled = true
cron_utc = "0 0 0 * * * *"
retry_max_attempts = 1
retry_initial_backoff_secs = 1
retry_max_backoff_secs = 1
"#;
        let custom_path = "/etc/td-hub/sources.toml";
        let err = parse(raw, custom_path).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains(custom_path),
            "expected {custom_path:?} in message, got {msg:?}",
        );
    }

    #[test]
    fn zero_attempts_rejected() {
        let raw = r#"
[sources.data_gov_tw]
enabled = true
cron_utc = "0 0 0 * * * *"
retry_max_attempts = 0
retry_initial_backoff_secs = 1
retry_max_backoff_secs = 1
"#;
        let err = parse(raw, "test").unwrap_err();
        assert!(matches!(
            err,
            SourceConfigError::InvalidAttempts { value: 0, .. }
        ));
    }

    #[test]
    fn too_many_attempts_rejected() {
        // Caps `retry_max_attempts` at MAX_RETRY_ATTEMPTS
        // so the DLQ's INTEGER (i32) column can't overflow
        // and a misconfigured deploy can't accidentally
        // schedule billions of retries.
        let raw = r#"
[sources.data_gov_tw]
enabled = true
cron_utc = "0 0 0 * * * *"
retry_max_attempts = 4000000000
retry_initial_backoff_secs = 1
retry_max_backoff_secs = 1
"#;
        let err = parse(raw, "test").unwrap_err();
        assert!(matches!(err, SourceConfigError::InvalidAttempts { .. }));
    }

    #[test]
    fn backoff_seconds_above_limit_rejected() {
        // The retry envelope's `Duration::checked_mul(2)`
        // saturates on overflow, but boot-time validation
        // catches the misconfig much louder. 24 h is the
        // upper bound; 25 h trips it.
        let raw = r#"
[sources.data_gov_tw]
enabled = true
cron_utc = "0 0 0 * * * *"
retry_max_attempts = 3
retry_initial_backoff_secs = 1
retry_max_backoff_secs = 90000
"#;
        let err = parse(raw, "test").unwrap_err();
        assert!(
            matches!(err, SourceConfigError::BackoffTooLarge { .. }),
            "got {err:?}",
        );
    }

    #[test]
    fn zero_backoff_with_retries_rejected() {
        // Tight retry loop guard: zero initial backoff +
        // multiple attempts would hammer upstream.
        let raw = r#"
[sources.data_gov_tw]
enabled = true
cron_utc = "0 0 0 * * * *"
retry_max_attempts = 3
retry_initial_backoff_secs = 0
retry_max_backoff_secs = 10
"#;
        let err = parse(raw, "test").unwrap_err();
        assert!(
            matches!(
                err,
                SourceConfigError::ZeroBackoffWithRetries {
                    attempts: 3,
                    initial: 0,
                    ..
                },
            ),
            "got {err:?}",
        );
    }

    #[test]
    fn zero_backoff_with_single_attempt_allowed() {
        // The intentional "no retry" mode: max_attempts =
        // 1 makes the envelope a single call, so the
        // backoff value is unused — zero is fine there.
        let raw = r#"
[sources.data_gov_tw]
enabled = true
cron_utc = "0 0 0 * * * *"
retry_max_attempts = 1
retry_initial_backoff_secs = 0
retry_max_backoff_secs = 0
"#;
        let configs = parse(raw, "test").expect("parse single-attempt");
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].retry.max_attempts, 1);
    }

    #[test]
    fn backoff_out_of_order_rejected() {
        let raw = r#"
[sources.data_gov_tw]
enabled = true
cron_utc = "0 0 0 * * * *"
retry_max_attempts = 3
retry_initial_backoff_secs = 100
retry_max_backoff_secs = 50
"#;
        let err = parse(raw, "test").unwrap_err();
        assert!(matches!(
            err,
            SourceConfigError::BackoffOutOfOrder {
                initial: 100,
                max: 50,
                ..
            }
        ));
    }

    #[test]
    fn disabled_section_skips_semantic_validation() {
        let raw = r#"
[sources.twse]
enabled = false
cron_utc = "bogus expression"
retry_max_attempts = 99999
retry_initial_backoff_secs = 9999999
retry_max_backoff_secs = 1
"#;
        // Disabled sources skip *semantic* checks
        // (cron syntax, attempt/backoff bounds), so a
        // known-broken row can stay as a placeholder
        // for a connector that hasn't landed yet.
        // Structural TOML parse still has to succeed —
        // see `disabled_section_with_missing_field_rejected`
        // for the boundary.
        let configs = parse(raw, "test").expect("parse disabled-only");
        assert!(configs.is_empty());
    }

    #[test]
    fn disabled_section_with_missing_field_still_rejected() {
        // Disabled rows still need to PARSE — missing
        // required fields fails at TOML deserialisation
        // (Serde requires the fields). Pin this so the
        // loader doc that says "disabled rows skip
        // validation" doesn't get misread as "disabled
        // rows skip parsing".
        let raw = r"
[sources.twse]
enabled = false
";
        let err = parse(raw, "test").unwrap_err();
        assert!(
            matches!(err, SourceConfigError::Parse { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn malformed_toml_rejected() {
        let err = parse("not valid {{ toml", "test").unwrap_err();
        assert!(matches!(err, SourceConfigError::Parse { .. }));
    }

    /// Verify the checked-in `config/sources.toml` parses
    /// cleanly with the real schema. Catches drift
    /// between the file and the loader at `cargo test`
    /// rather than at worker boot.
    #[test]
    fn checked_in_sources_toml_loads() {
        let raw = include_str!("../../../config/sources.toml");
        let configs = parse(raw, "config/sources.toml").expect("checked-in file parses");
        // Production has data.gov.tw + TWSE + MOEA + CWA
        // enabled today (M5b.5 flips Fishery on when its
        // connector lands). A future PR that turns on the
        // remaining source should update this assertion in
        // lockstep so the test is the spec.
        let enabled_ids: Vec<SourceId> = configs.iter().map(|c| c.source_id).collect();
        assert_eq!(configs.len(), 4, "{configs:?}");
        assert!(enabled_ids.contains(&SourceId::DataGovTw));
        assert!(enabled_ids.contains(&SourceId::Twse));
        assert!(enabled_ids.contains(&SourceId::Moea));
        assert!(enabled_ids.contains(&SourceId::Cwa));
    }
}
