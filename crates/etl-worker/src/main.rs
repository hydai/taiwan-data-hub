//! cron-driven ETL worker that mirrors upstream data sources.
//!
//! The registry of sources, schedules, and retry policies lives in
//! `config/sources.toml`. Each enabled source gets one cron job
//! registered with `tokio-cron-scheduler`; jobs run independently
//! and their crawl passes go through the retry-with-backoff
//! envelope. On terminal failure the envelope writes an `etl_dlq`
//! row so operators can read a single table to find sources that
//! need manual attention.
//!
//! The cache pipeline (#3.6) keeps its independent 6-hour cron — it
//! isn't a "source", it's a maintenance task.
//!
//! Configuration is via environment variables:
//!
//! | env                       | required | default                                  |
//! |---------------------------|----------|------------------------------------------|
//! | `DATABASE_URL`            | yes      | —                                        |
//! | `DATA_GOV_TW_URL`         | no       | `https://data.gov.tw`                    |
//! | `SOURCES_CONFIG_PATH`     | no       | `config/sources.toml`                    |
//! | `ETL_DB_MAX_CONNECTIONS`  | no       | `20` (must be a positive integer if set) |
//! | `ETL_RUN_AT_STARTUP`      | no       | `false`                                  |
//!
//! When `ETL_RUN_AT_STARTUP=true` the worker runs one immediate
//! pass per enabled source before settling into cron.

mod cache_pipeline;
mod driver;
mod retry;
mod sources;

use std::env;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use connectors::{SourceConnector, SourceId, data_gov_tw::DataGovTwConnector};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use storage::{CacheState, DlqErrorKind, EtlDlqRepo, NewDlqEntry, Storage};
use tokio_cron_scheduler::{Job, JobScheduler};
use tracing_subscriber::EnvFilter;

use crate::cache_pipeline::{CacheTickConfig, run_cache_tick};
use crate::driver::{CrawlError, run_one_pass};
use crate::retry::{RetryConfig, RetryOutcome, dlq_error_kind, with_retry};
use crate::sources::SourceConfig;

/// #3.6 hot-cache pipeline tick: every 6 hours at the top of the
/// hour (UTC 00:00 / 06:00 / 12:00 / 18:00). Independent of the
/// source-registry cadence — promotion / demotion catches up to
/// query traffic without churning the catalog.
const CACHE_TICK_CRON: &str = "0 0 0,6,12,18 * * * *";

const DEFAULT_SOURCES_PATH: &str = "config/sources.toml";

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

    let sources_path = read_optional_env("SOURCES_CONFIG_PATH")?
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_SOURCES_PATH.to_owned());
    let configs = sources::load(&sources_path)
        .with_context(|| format!("could not load source registry from {sources_path}"))?;
    if configs.is_empty() {
        tracing::warn!(
            sources_path,
            "no ETL sources enabled — only the cache pipeline will run",
        );
    } else {
        tracing::info!(
            sources_path,
            count = configs.len(),
            "loaded enabled ETL sources",
        );
    }

    let mut scheduler = JobScheduler::new()
        .await
        .context("could not start cron scheduler")?;

    // Build each enabled connector + register its cron job. A
    // source that's enabled in the TOML but unimplemented in
    // `build_connector_for` fails boot — much louder than a
    // silently-skipped crawl.
    for cfg in &configs {
        let connector = build_connector_for(cfg.source_id)?;
        register_source_job(&mut scheduler, *cfg, connector, storage.clone()).await?;
    }

    if run_at_startup()? {
        tracing::info!("ETL_RUN_AT_STARTUP=true; running one immediate pass per source");
        for cfg in &configs {
            let connector = build_connector_for(cfg.source_id)?;
            crawl_with_retry_and_dlq(connector, storage.clone(), cfg.retry, cfg.source_id).await;
        }
    }

    // Cache tick — independent of the source registry.
    let cache_state: Arc<dyn CacheState> = Arc::new(storage.clone());
    let cache_state_for_cron = cache_state.clone();
    let cache_job = Job::new_async_tz(CACHE_TICK_CRON, Utc, move |_uuid, _l| {
        let state = cache_state_for_cron.clone();
        Box::pin(async move {
            tracing::info!("cron tick: running cache pipeline (#3.6)");
            match run_cache_tick(state, CacheTickConfig::default()).await {
                Ok(report) => {
                    tracing::info!(
                        hot_candidate_count = report.hot_candidate_count,
                        demoted_count = report.demoted_count,
                        hit_ratio = ?report.hit_ratio(),
                        "cache tick complete",
                    );
                }
                Err(e) => tracing::error!(error = %e, "cache tick failed"),
            }
        })
    })
    .context("failed to construct cache tick job")?;
    scheduler
        .add(cache_job)
        .await
        .context("failed to register cache tick job")?;

    scheduler
        .start()
        .await
        .context("failed to start scheduler")?;

    tracing::info!(
        cache_cron_utc = CACHE_TICK_CRON,
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

/// Construct the connector for one [`SourceId`]. Adding a new
/// connector is: implement the trait in `crates/connectors`, add
/// a case here, and flip `enabled = true` in `config/sources.toml`.
fn build_connector_for(id: SourceId) -> Result<Arc<dyn SourceConnector>> {
    match id {
        SourceId::DataGovTw => {
            let c = build_data_gov_tw_connector()?;
            Ok(Arc::new(c) as Arc<dyn SourceConnector>)
        }
        SourceId::Twse | SourceId::Moea | SourceId::Cwa | SourceId::FisheryMoa => {
            // M5b.2–M5b.5 add cases here as connectors land. Until
            // then, an `enabled = true` row for an unimplemented
            // source fails boot loudly — better than a silently-
            // skipped crawl.
            anyhow::bail!(
                "{id} connector is not yet implemented (see #5b.2–#5b.5); \
                 set sources.{id}.enabled = false in config/sources.toml"
            )
        }
        SourceId::UserContrib => {
            anyhow::bail!("user_contrib is not ETL-driven; remove it from sources.toml")
        }
    }
}

/// Register one cron job for an enabled source. The closure
/// captures clones of the connector + storage so the scheduler can
/// outlive the registration call. `crawl_with_retry_and_dlq`
/// handles the actual work.
async fn register_source_job(
    scheduler: &mut JobScheduler,
    cfg: SourceConfig,
    connector: Arc<dyn SourceConnector>,
    storage: Storage,
) -> Result<()> {
    let job = Job::new_async_tz(cfg.cron_utc, Utc, move |_uuid, _l| {
        let storage = storage.clone();
        let connector = connector.clone();
        Box::pin(async move {
            tracing::info!(source = %cfg.source_id, "cron tick: starting crawl");
            crawl_with_retry_and_dlq(connector, storage, cfg.retry, cfg.source_id).await;
        })
    })
    .with_context(|| format!("failed to construct cron job for {}", cfg.source_id))?;
    scheduler
        .add(job)
        .await
        .with_context(|| format!("failed to register cron job for {}", cfg.source_id))?;
    tracing::info!(
        source = %cfg.source_id,
        cron_utc = cfg.cron_utc,
        retry_max_attempts = cfg.retry.max_attempts,
        "ETL source scheduled",
    );
    Ok(())
}

/// Run one crawl pass through the retry envelope. On terminal
/// failure, write an `etl_dlq` row.
async fn crawl_with_retry_and_dlq(
    connector: Arc<dyn SourceConnector>,
    storage: Storage,
    retry_cfg: RetryConfig,
    source_id: SourceId,
) {
    let outcome = with_retry(
        retry_cfg,
        || {
            let connector = connector.clone();
            let storage = storage.clone();
            async move {
                match run_one_pass(&*connector, &storage).await {
                    Ok(summary) => {
                        tracing::info!(source = %source_id, ?summary, "crawl pass complete");
                        Ok(summary)
                    }
                    // `CrawlError::Connector` carries the original
                    // `ConnectorError` — bubble it so the
                    // classifier can decide retriable-or-not.
                    Err(CrawlError::Connector(e)) => Err(e),
                    // Storage failures are terminal: failing once
                    // means our DB is unhealthy, and retrying the
                    // upstream crawl would re-issue the same write
                    // and fail again. Map to `Config` so the
                    // classifier short-circuits to the DLQ path.
                    Err(CrawlError::Storage(e)) => Err(connectors::ConnectorError::Config(
                        format!("storage failure during crawl: {e}"),
                    )),
                }
            }
        },
        tokio::time::sleep,
    )
    .await;
    if let RetryOutcome::Err { error, attempts } = outcome {
        tracing::error!(
            source = %source_id,
            attempts,
            error = %error,
            "crawl pass failed after retries; writing to etl_dlq",
        );
        let error_kind =
            DlqErrorKind::from_wire(dlq_error_kind(&error)).unwrap_or(DlqErrorKind::Other);
        let entry = NewDlqEntry {
            source: source_id.as_str().to_string(),
            job_kind: "list_datasets".to_string(),
            attempts: i32::try_from(attempts).unwrap_or(i32::MAX),
            error_kind,
            error_message: format!("{error}"),
            payload: Some(json!({})),
        };
        if let Err(e) = EtlDlqRepo::insert(&storage, entry).await {
            tracing::error!(error = %e, "could not write DLQ row");
        }
    }
}

/// Default pool size — wider than `Storage::connect`'s gateway-tuned
/// 5 because the binary runs multiple connectors in parallel and
/// each crawl already alternates between dataset upserts and domain-
/// id lookups against the same pool. 20 leaves headroom for the
/// multi-connector scheduler without monopolising connection slots
/// when the gateway and the worker share a Postgres.
const DEFAULT_ETL_MAX_CONNECTIONS: u32 = 20;

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
    let override_url = read_optional_env("DATA_GOV_TW_URL")?
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    if let Some(url) = override_url {
        builder = builder.base_url(url);
    }
    builder
        .build()
        .context("could not build data.gov.tw connector")
}

fn run_at_startup() -> Result<bool> {
    Ok(read_optional_env("ETL_RUN_AT_STARTUP")?
        .as_deref()
        .map(str::trim)
        .is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes")))
}

fn read_optional_env(name: &str) -> Result<Option<String>> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => {
            anyhow::bail!("{name} is set but contains non-UTF-8 bytes")
        }
    }
}

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
        assert!(parse_max_connections(Some("-1".into())).is_err());
    }

    /// `build_connector_for` is the choke point that gates an
    /// `enabled = true` source against an actually-implemented
    /// connector. Today only `DataGovTw` is implemented; the
    /// others must error loudly so a sources.toml typo or a
    /// premature flip can't silently drop a crawl.
    #[test]
    fn build_connector_only_data_gov_tw_succeeds() {
        // DataGovTw is the only enabled-AND-implemented source
        // in the checked-in sources.toml.
        assert!(build_connector_for(SourceId::DataGovTw).is_ok());
        // M5b.2-5 will turn these into `Ok` as their connectors
        // land; flipping the assertion in lockstep keeps the
        // test the spec.
        assert!(build_connector_for(SourceId::Twse).is_err());
        assert!(build_connector_for(SourceId::Moea).is_err());
        assert!(build_connector_for(SourceId::Cwa).is_err());
        assert!(build_connector_for(SourceId::FisheryMoa).is_err());
        // user_contrib is never ETL-driven.
        assert!(build_connector_for(SourceId::UserContrib).is_err());
    }
}
