//! cron-driven ETL worker that mirrors upstream data sources.
//!
//! Schedule: nightly at **02:00 Asia/Taipei** (per docs/DESIGN.md §4.4).
//! The cron expression is interpreted in UTC because `tokio-cron-scheduler`
//! evaluates against the system clock; the TPE-local 02:00 maps to
//! 18:00 UTC year-round (Taiwan does not observe DST).
//!
//! Configuration is via environment variables:
//!
//! | env                  | required | default                       |
//! |----------------------|----------|-------------------------------|
//! | `DATABASE_URL`       | yes      | —                             |
//! | `DATA_GOV_TW_URL`    | no       | `https://data.gov.tw`         |
//! | `ETL_RUN_AT_STARTUP` | no       | `false`                       |
//!
//! When `ETL_RUN_AT_STARTUP=true` (handy for local dev / CI smoke
//! tests) the worker runs a single immediate pass before settling
//! into the cron loop.

mod driver;

use std::env;

use anyhow::{Context, Result};
use connectors::data_gov_tw::DataGovTwConnector;
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
    let storage = Storage::connect(&database_url)
        .await
        .context("storage::connect failed — is Postgres reachable?")?;

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

fn build_data_gov_tw_connector() -> Result<DataGovTwConnector> {
    let mut builder = DataGovTwConnector::builder();
    if let Ok(url) = env::var("DATA_GOV_TW_URL") {
        builder = builder.base_url(url);
    }
    builder
        .build()
        .map_err(|e| anyhow::anyhow!("could not build data.gov.tw connector: {e}"))
}

fn run_at_startup() -> bool {
    env::var("ETL_RUN_AT_STARTUP")
        .ok()
        .as_deref()
        .is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}

/// Sleep until SIGTERM or SIGINT arrives so the scheduler can run
/// indefinitely under `docker compose` / systemd / k8s.
async fn wait_for_shutdown() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM");
    let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT");
    tokio::select! {
        _ = sigterm.recv() => tracing::info!("received SIGTERM"),
        _ = sigint.recv() => tracing::info!("received SIGINT"),
    }
}

// main.rs is glue. Crawl driver logic lives in `driver.rs` and is
// covered by the testcontainers + wiremock integration test there;
// the cron string is validated at process boot when the scheduler
// rejects malformed expressions.
