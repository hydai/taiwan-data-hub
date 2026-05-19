//! cron-driven ETL worker that mirrors upstream data sources.
//!
//! Schedule: nightly at **02:00 Asia/Taipei** (per docs/DESIGN.md §4.4).
//! The cron expression is interpreted in UTC because `tokio-cron-scheduler`
//! evaluates against the system clock; the TPE-local 02:00 maps to
//! 18:00 UTC year-round (Taiwan does not observe DST).
//!
//! Configuration is via environment variables:
//!
//! | env                       | required | default                       |
//! |---------------------------|----------|-------------------------------|
//! | `DATABASE_URL`            | yes      | —                             |
//! | `DATA_GOV_TW_URL`         | no       | `https://data.gov.tw`         |
//! | `ETL_DB_MAX_CONNECTIONS`  | no       | `20`                          |
//! | `ETL_RUN_AT_STARTUP`      | no       | `false`                       |
//!
//! When `ETL_RUN_AT_STARTUP=true` (handy for local dev / CI smoke
//! tests) the worker runs a single immediate pass before settling
//! into the cron loop.

mod driver;

use std::env;

use anyhow::{Context, Result};
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

    if run_at_startup() {
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
    let job = Job::new_async(NIGHTLY_TPE_2AM_IN_UTC, move |_uuid, _l| {
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
        "ETL worker scheduled; waiting for SIGTERM / SIGINT"
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
    let max_connections = env::var("ETL_DB_MAX_CONNECTIONS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_ETL_MAX_CONNECTIONS);
    let pool = PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(database_url)
        .await
        .context("could not connect ETL pool — is Postgres reachable?")?;
    tracing::info!(max_connections, "ETL pool ready");
    Ok(Storage::from_pool(pool))
}

fn build_data_gov_tw_connector() -> Result<DataGovTwConnector> {
    let mut builder = DataGovTwConnector::builder();
    if let Ok(url) = env::var("DATA_GOV_TW_URL") {
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

fn run_at_startup() -> bool {
    env::var("ETL_RUN_AT_STARTUP")
        .ok()
        .as_deref()
        .is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}

/// Sleep until a shutdown signal arrives so the scheduler can run
/// indefinitely under `docker compose` / systemd / k8s, or Windows
/// (the README still lists Windows in the Quickstart).
///
/// Unix gets the rich SIGTERM/SIGINT pair (so `docker stop` SIGTERM
/// drains cleanly). Other platforms fall back to `ctrl_c()` which
/// covers SIGINT-equivalent shutdown without pulling in
/// `tokio::signal::unix` (which only compiles on Unix).
async fn wait_for_shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM");
        let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT");
        tokio::select! {
            _ = sigterm.recv() => tracing::info!("received SIGTERM"),
            _ = sigint.recv() => tracing::info!("received SIGINT"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("received Ctrl-C");
    }
}

// main.rs is glue. Crawl driver logic lives in `driver.rs` and is
// covered by the testcontainers + wiremock integration test there;
// the cron string is validated at process boot when the scheduler
// rejects malformed expressions.
