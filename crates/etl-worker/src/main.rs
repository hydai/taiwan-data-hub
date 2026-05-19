//! cron-driven ETL worker that mirrors upstream data sources.
//!
//! Schedule: nightly at **02:00 Asia/Taipei** (per docs/DESIGN.md §4.4).
//! The cron expression is hand-converted to UTC (`0 0 18 * * * *` =
//! 18:00 UTC = 02:00 TPE; Taiwan does not observe DST) and passed
//! through [`tokio_cron_scheduler::Job::new_async_tz`] with an explicit
//! [`chrono::Utc`] timezone so the schedule is decoupled from whatever
//! local timezone the host process happens to inherit. (`new_async`
//! also defaults to `Utc` in 0.15.1, but the call site is more
//! defensive against future API changes if it spells the timezone
//! out.)
//!
//! Configuration is via environment variables:
//!
//! | env                       | required | default                                  |
//! |---------------------------|----------|------------------------------------------|
//! | `DATABASE_URL`            | yes      | —                                        |
//! | `DATA_GOV_TW_URL`         | no       | `https://data.gov.tw`                    |
//! | `ETL_DB_MAX_CONNECTIONS`  | no       | `20` (must be a positive integer if set) |
//! | `ETL_RUN_AT_STARTUP`      | no       | `false`                                  |
//!
//! When `ETL_RUN_AT_STARTUP=true` (handy for local dev / CI smoke
//! tests) the worker runs a single immediate pass before settling
//! into the cron loop.

mod driver;

use std::env;

use anyhow::{Context, Result};
use chrono::Utc;
use connectors::data_gov_tw::DataGovTwConnector;
use sqlx::postgres::PgPoolOptions;
use storage::Storage;
use tokio_cron_scheduler::{Job, JobScheduler};
use tracing_subscriber::EnvFilter;

use crate::driver::run_one_pass;

/// "Nightly at 02:00 Asia/Taipei" expressed in UTC. Taiwan does not
/// observe DST so this is fixed year-round (TPE = UTC+8 → 02:00 TPE
/// = 18:00 UTC the previous calendar day).
///
/// Format is the 7-field cron spec `tokio-cron-scheduler` expects:
/// `sec min hour day-of-month month day-of-week year`.
const NIGHTLY_TPE_2AM_IN_UTC: &str = "0 0 18 * * * *";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_target(false)
        .compact()
        .init();

    let database_url =
        env::var("DATABASE_URL").context("DATABASE_URL is required for the ETL worker")?;
    let storage = connect_storage(&database_url).await?;

    let connector = build_data_gov_tw_connector()?;

    if run_at_startup()? {
        tracing::info!("ETL_RUN_AT_STARTUP=true; running an immediate pass");
        match run_one_pass(&connector, &storage).await {
            Ok(summary) => tracing::info!(?summary, "startup pass complete"),
            Err(e) => tracing::error!(error = %e, "startup pass failed; continuing into cron"),
        }
    }

    let mut scheduler = JobScheduler::new()
        .await
        .context("could not start cron scheduler")?;

    // The job closure captures `storage` + `connector` (both cheap to
    // clone — pool is Arc-backed, connector holds a reqwest::Client).
    let storage_for_cron = storage.clone();
    let connector_for_cron = connector.clone();
    let job = Job::new_async_tz(NIGHTLY_TPE_2AM_IN_UTC, Utc, move |_uuid, _l| {
        let storage = storage_for_cron.clone();
        let connector = connector_for_cron.clone();
        Box::pin(async move {
            tracing::info!("cron tick: starting nightly crawl");
            match run_one_pass(&connector, &storage).await {
                Ok(summary) => tracing::info!(?summary, "nightly crawl complete"),
                Err(e) => tracing::error!(error = %e, "nightly crawl failed"),
            }
        })
    })
    .context("failed to construct cron job")?;
    scheduler.add(job).await.context("failed to register job")?;
    scheduler
        .start()
        .await
        .context("failed to start scheduler")?;

    tracing::info!(
        cron_utc = NIGHTLY_TPE_2AM_IN_UTC,
        "ETL worker scheduled; waiting for shutdown signal"
    );

    wait_for_shutdown().await;
    scheduler
        .shutdown()
        .await
        .context("scheduler shutdown failed")?;
    tracing::info!("ETL worker stopped");
    Ok(())
}

/// Default pool size — wider than `Storage::connect`'s gateway-tuned
/// 5 because the binary may eventually run multiple connectors in
/// parallel (#5b adds TWSE / MOEA / CWA / Fishery) and each crawl
/// already alternates between dataset upserts and domain-id lookups
/// against the same pool. Today `run_one_pass` processes datasets
/// sequentially, so 5 would also work; 20 leaves headroom for the
/// multi-connector scheduler without monopolising connection slots
/// when the gateway and the worker share a Postgres.
const DEFAULT_ETL_MAX_CONNECTIONS: u32 = 20;

/// Construct the ETL-tuned `Storage`. `Storage::connect` is sized for
/// the gateway (`max_connections`=5) and explicitly says the crawler
/// should build its own pool via `from_pool`. Honour the contract:
/// read `ETL_DB_MAX_CONNECTIONS` (default 20), build a `PgPool`, and
/// wrap it.
async fn connect_storage(database_url: &str) -> Result<Storage> {
    let max_connections = parse_max_connections(read_optional_env("ETL_DB_MAX_CONNECTIONS")?)?;
    let pool = PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(database_url)
        .await
        .context("could not connect ETL pool — is Postgres reachable?")?;
    tracing::info!(max_connections, "ETL pool ready");
    Ok(Storage::from_pool(pool))
}

/// Strict parser for `ETL_DB_MAX_CONNECTIONS`. Fails fast on any
/// present-but-malformed value — non-numeric, zero, negative — so a
/// misconfigured deploy surfaces at boot instead of as a silently
/// undersized pool (or, for `0`, a pool that deadlocks on first
/// `acquire`). Unset, empty, or whitespace-only values fall back to
/// `DEFAULT_ETL_MAX_CONNECTIONS`: Docker / k8s templates frequently
/// emit `ETL_DB_MAX_CONNECTIONS=` for unsupplied optional overrides,
/// and rejecting that would punish benign deployments.
fn parse_max_connections(raw: Option<String>) -> Result<u32> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_ETL_MAX_CONNECTIONS);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(DEFAULT_ETL_MAX_CONNECTIONS);
    }
    let parsed: u32 = trimmed
        .parse()
        .with_context(|| format!("ETL_DB_MAX_CONNECTIONS={trimmed:?} is not a positive integer"))?;
    if parsed == 0 {
        anyhow::bail!("ETL_DB_MAX_CONNECTIONS must be > 0 (got 0)");
    }
    Ok(parsed)
}

fn build_data_gov_tw_connector() -> Result<DataGovTwConnector> {
    let mut builder = DataGovTwConnector::builder();
    // Trim + empty-skip the override so `DATA_GOV_TW_URL=` (empty
    // value, common in Docker / k8s optional-var templating) behaves
    // like "unset" rather than passing `""` to `base_url` and failing
    // URL parsing at boot. Same shape as `parse_max_connections`.
    // `read_optional_env` bubbles up `VarError::NotUnicode` so a
    // garbled value doesn't silently degrade to the default URL.
    let override_url = read_optional_env("DATA_GOV_TW_URL")?
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    if let Some(url) = override_url {
        builder = builder.base_url(url);
    }
    // `.context` preserves `BuildError` as the underlying source so
    // anyhow's chain reporting surfaces "invalid URL" vs. "reqwest
    // client build failed" verbatim. A bare `anyhow!(\"...: {e}\")`
    // flattens that chain into a string and drops the root cause.
    builder
        .build()
        .context("could not build data.gov.tw connector")
}

fn run_at_startup() -> Result<bool> {
    Ok(read_optional_env("ETL_RUN_AT_STARTUP")?
        .as_deref()
        .is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes")))
}

/// Read an optional env var, distinguishing the three states the
/// caller usually cares about:
///
/// - `Ok(None)` — not set; caller should fall back to a default
/// - `Ok(Some(s))` — set to a valid UTF-8 string `s`
/// - `Err(_)` — set, but the value isn't valid UTF-8
///
/// `env::var(...).ok()` collapses the third case into the first
/// (silently using the default for a garbled value), which weakens
/// the "fail fast on misconfig" contract this binary leans on.
fn read_optional_env(name: &str) -> Result<Option<String>> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => {
            anyhow::bail!("{name} is set but contains non-UTF-8 bytes")
        }
    }
}

/// Sleep until a shutdown signal arrives so the scheduler can run
/// indefinitely under `docker compose` / systemd / k8s, or Windows
/// (the README still lists Windows in the Quickstart).
///
/// Unix gets the rich SIGTERM/SIGINT pair (so `docker stop` SIGTERM
/// drains cleanly). If installing those handlers fails — possible
/// inside containers that have dropped `CAP_KILL` or similarly
/// restricted capabilities — we log the failure and fall back to
/// `ctrl_c()` so the worker still has *some* shutdown signal rather
/// than crashing at boot. The non-Unix path always uses `ctrl_c()`.
///
/// `ctrl_c()` itself returns a `Result`; an error there is rare
/// (e.g. the signal stream became unhealthy) but we surface it as
/// an error log and still proceed to shutdown — keeping the worker
/// hanging on a broken watcher would be worse than exiting cleanly.
async fn wait_for_shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
        ) {
            (Ok(mut sigterm), Ok(mut sigint)) => {
                tokio::select! {
                    _ = sigterm.recv() => tracing::info!("received SIGTERM"),
                    _ = sigint.recv() => tracing::info!("received SIGINT"),
                }
                return;
            }
            (Err(e), _) | (_, Err(e)) => {
                tracing::warn!(
                    error = %e,
                    "could not install Unix signal handlers; falling back to Ctrl-C",
                );
            }
        }
    }
    match tokio::signal::ctrl_c().await {
        Ok(()) => tracing::info!("received Ctrl-C"),
        Err(e) => tracing::error!(
            error = %e,
            "ctrl_c watcher failed; treating as shutdown to exit cleanly",
        ),
    }
}

// main.rs is glue. Crawl driver logic lives in `driver.rs` and is
// covered by the testcontainers + wiremock integration test there;
// the cron string is validated at process boot when the scheduler
// rejects malformed expressions. The env-var parser below has its
// own unit tests so the contract is verifiable without spinning up
// a tokio runtime or a Postgres container.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_yields_default() {
        assert_eq!(
            parse_max_connections(None).unwrap(),
            DEFAULT_ETL_MAX_CONNECTIONS,
        );
    }

    #[test]
    fn empty_or_whitespace_yields_default() {
        assert_eq!(
            parse_max_connections(Some(String::new())).unwrap(),
            DEFAULT_ETL_MAX_CONNECTIONS,
        );
        assert_eq!(
            parse_max_connections(Some("   ".into())).unwrap(),
            DEFAULT_ETL_MAX_CONNECTIONS,
        );
    }

    #[test]
    fn positive_integer_passes_through() {
        assert_eq!(parse_max_connections(Some("42".into())).unwrap(), 42);
        // Surrounding whitespace is tolerated (shell + YAML often leak it).
        assert_eq!(parse_max_connections(Some("  7 ".into())).unwrap(), 7);
    }

    #[test]
    fn zero_is_rejected() {
        let err = parse_max_connections(Some("0".into())).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("must be > 0"), "unexpected error: {msg}");
    }

    #[test]
    fn non_numeric_is_rejected() {
        let err = parse_max_connections(Some("twenty".into())).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("ETL_DB_MAX_CONNECTIONS"),
            "unexpected error: {msg}"
        );
        assert!(msg.contains("positive integer"), "unexpected error: {msg}");
    }

    #[test]
    fn negative_is_rejected() {
        // `u32::from_str` rejects "-1" outright, so it lands in the same
        // parse-error path as non-numeric input.
        assert!(parse_max_connections(Some("-1".into())).is_err());
    }

    // `read_optional_env` isn't unit-tested directly: exercising the
    // `NotUnicode` branch needs `std::env::{set_var, remove_var}` which
    // are `unsafe` under Rust 2024, and the workspace forbids unsafe.
    // The function's contract is small enough to verify by inspection,
    // and `connect_storage` / `build_data_gov_tw_connector` exercise
    // the happy path on every boot.
}
