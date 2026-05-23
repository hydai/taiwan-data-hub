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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceConfig {
    pub source_id: SourceId,
    /// 7-field UTC cron expression passed to
    /// `Job::new_async_tz`.
    pub cron_utc: &'static str,
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
    #[error("unknown source {name:?} — not in datasets.source enum")]
    UnknownSource { name: String },
    #[error("source {name:?}: retry_max_attempts must be ≥ 1 (got {value})")]
    InvalidAttempts { name: String, value: u32 },
    #[error(
        "source {name:?}: retry_initial_backoff_secs ({initial}) must be ≤ retry_max_backoff_secs ({max})"
    )]
    BackoffOutOfOrder {
        name: String,
        initial: u64,
        max: u64,
    },
}

/// Read `config/sources.toml` and return one
/// [`SourceConfig`] per enabled source. Disabled sources
/// are silently dropped — the per-source row stays in the
/// file as documentation but doesn't get scheduled.
///
/// `cron_utc` strings are NOT validated here — the cron
/// scheduler is the authority on expression syntax, and
/// it surfaces a clear error at registration time.
/// Re-validating in two places invites drift.
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
        let source_id = source_id_from_name(&name)
            .ok_or_else(|| SourceConfigError::UnknownSource { name: name.clone() })?;
        if entry.retry_max_attempts < 1 {
            return Err(SourceConfigError::InvalidAttempts {
                name,
                value: entry.retry_max_attempts,
            });
        }
        if entry.retry_initial_backoff_secs > entry.retry_max_backoff_secs {
            return Err(SourceConfigError::BackoffOutOfOrder {
                name,
                initial: entry.retry_initial_backoff_secs,
                max: entry.retry_max_backoff_secs,
            });
        }
        // Lift the cron string into &'static — necessary
        // because `Job::new_async_tz` wants `&str` and the
        // cron job closure outlives the loader's
        // BTreeMap. `Box::leak` is appropriate at boot
        // for a bounded number of sources (≤ 6 today).
        let cron_utc: &'static str = Box::leak(entry.cron_utc.into_boxed_str());
        out.push(SourceConfig {
            source_id,
            cron_utc,
            retry: RetryConfig {
                max_attempts: entry.retry_max_attempts,
                initial_backoff: Duration::from_secs(entry.retry_initial_backoff_secs),
                max_backoff: Duration::from_secs(entry.retry_max_backoff_secs),
            },
        });
    }
    Ok(out)
}

/// `sources.toml` section name → `SourceId`. The names
/// must match `datasets.source`'s SQL CHECK enum exactly
/// — `data_gov_tw`, `twse`, `moea`, `cwa`, `fishery_moa`,
/// `user_contrib`. `user_contrib` isn't ETL-driven so it
/// isn't a valid sources.toml section.
fn source_id_from_name(name: &str) -> Option<SourceId> {
    match name {
        "data_gov_tw" => Some(SourceId::DataGovTw),
        "twse" => Some(SourceId::Twse),
        "moea" => Some(SourceId::Moea),
        "cwa" => Some(SourceId::Cwa),
        "fishery_moa" => Some(SourceId::FisheryMoa),
        _ => None,
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
        let c = configs[0];
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
    fn disabled_section_does_not_block_load() {
        let raw = r#"
[sources.twse]
enabled = false
cron_utc = "bogus expression"
retry_max_attempts = 99999
retry_initial_backoff_secs = 9999999
retry_max_backoff_secs = 1
"#;
        // Disabled sources are skipped entirely — invalid
        // values inside don't poison the load. This is
        // deliberate: it lets an operator leave a known-
        // broken row as a placeholder for the connector
        // impl that hasn't landed yet.
        let configs = parse(raw, "test").expect("parse disabled-only");
        assert!(configs.is_empty());
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
        // Production starts with only data.gov.tw enabled
        // (M5b.2-5 flip the others on as their connectors
        // land). A future PR that turns on TWSE / MOEA /
        // CWA / Fishery should update this assertion in
        // lockstep so the test is the spec.
        assert_eq!(configs.len(), 1, "{configs:?}");
        assert_eq!(configs[0].source_id, SourceId::DataGovTw);
    }
}
