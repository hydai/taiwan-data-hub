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
use connectors::{SourceConnector, SourceId, data_gov_tw::DataGovTwConnector, twse::TwseConnector};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use storage::{CacheState, DlqErrorKind, EtlDlqRepo, NewDlqEntry, Storage};
use tokio_cron_scheduler::{Job, JobScheduler};
use tracing_subscriber::EnvFilter;

use crate::cache_pipeline::{CacheTickConfig, run_cache_tick};
use crate::driver::{CrawlError, run_one_pass};
use crate::retry::{RetryConfig, RetryOutcome, dlq_error_kind, log_friendly, with_retry};
use crate::sources::SourceConfig;

/// #3.6 hot-cache pipeline tick: every 6 hours at the top of the
/// hour (UTC 00:00 / 06:00 / 12:00 / 18:00). Independent of the
/// source-registry cadence — promotion / demotion catches up to
/// query traffic without churning the catalog.
const CACHE_TICK_CRON: &str = "0 0 0,6,12,18 * * * *";

const DEFAULT_SOURCES_PATH: &str = "config/sources.toml";

/// Prefix cap for the `etl_dlq.error_message` column.
/// Counts Unicode scalars, not UTF-16 code units, so a
/// CJK upstream that returns 2000 zh-TW characters lands
/// at exactly 2000 scalars (≈ 6000 bytes), not somewhere
/// mid-codepoint. Chosen to fit a typical upstream error
/// page's first paragraph without bloating the DLQ table
/// if the upstream returns megabytes. When truncation
/// fires, the stored value carries an extra
/// `…[truncated]` marker beyond this cap so an operator
/// can tell the row hit the limit — see
/// [`truncate_scalars`] for the exact post-marker shape.
const DLQ_MESSAGE_CHAR_LIMIT: usize = 2000;

/// Same prefix-cap semantics as
/// `DLQ_MESSAGE_CHAR_LIMIT`, applied to the
/// `BadStatus.body` excerpt stored in the DLQ payload —
/// smaller cap because the payload is best-effort
/// context, not the primary diagnostic.
const DLQ_PAYLOAD_BODY_CHAR_LIMIT: usize = 1000;

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

    // Build each enabled connector ONCE and reuse the same
    // `Arc<dyn SourceConnector>` for cron registration AND any
    // startup pass. A source that's enabled in the TOML but
    // unimplemented in `build_connector_for` fails boot — much
    // louder than a silently-skipped crawl.
    let mut built: Vec<(SourceConfig, Arc<dyn SourceConnector>)> =
        Vec::with_capacity(configs.len());
    for cfg in &configs {
        let connector = build_connector_for(cfg.source_id, &sources_path).await?;
        built.push((cfg.clone(), connector));
    }
    for (cfg, connector) in &built {
        register_source_job(
            &mut scheduler,
            cfg.clone(),
            connector.clone(),
            storage.clone(),
        )
        .await?;
    }

    if run_at_startup()? {
        tracing::info!("ETL_RUN_AT_STARTUP=true; running one immediate pass per source");
        for (cfg, connector) in &built {
            crawl_with_retry_and_dlq(connector.clone(), storage.clone(), cfg.retry, cfg.source_id)
                .await;
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
/// a case here, and flip `enabled = true` in the sources config
/// (default `config/sources.toml`, overridable via
/// `SOURCES_CONFIG_PATH`). `sources_path` is threaded through
/// so the unimplemented-source error names the file the
/// operator actually loaded, not a default that might not
/// apply to their deploy.
///
/// `async` because some connectors (TWSE) fetch policy
/// documents like `robots.txt` at construction — that's an
/// HTTP call, must happen in a tokio context.
async fn build_connector_for(id: SourceId, sources_path: &str) -> Result<Arc<dyn SourceConnector>> {
    match id {
        SourceId::DataGovTw => {
            let c = build_data_gov_tw_connector()?;
            Ok(Arc::new(c) as Arc<dyn SourceConnector>)
        }
        SourceId::Twse => {
            let c = TwseConnector::new()
                .await
                .context("could not build TWSE connector")?;
            Ok(Arc::new(c) as Arc<dyn SourceConnector>)
        }
        SourceId::Moea | SourceId::Cwa | SourceId::FisheryMoa => {
            // M5b.3–M5b.5 add cases here as connectors land. Until
            // then, an `enabled = true` row for an unimplemented
            // source fails boot loudly — better than a silently-
            // skipped crawl.
            anyhow::bail!(
                "{id} connector is not yet implemented (see #5b.3–#5b.5); \
                 set sources.{id}.enabled = false in {sources_path}"
            )
        }
        SourceId::UserContrib => {
            anyhow::bail!("user_contrib is not ETL-driven; remove it from {sources_path}")
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
    // Snapshot the small Copy fields for the post-
    // registration log (the closure consumes `cfg`).
    let source_id = cfg.source_id;
    let retry = cfg.retry;
    let cron_utc_for_log = cfg.cron_utc.clone();
    let job = Job::new_async_tz(cfg.cron_utc.as_str(), Utc, move |_uuid, _l| {
        let storage = storage.clone();
        let connector = connector.clone();
        Box::pin(async move {
            tracing::info!(source = %source_id, "cron tick: starting crawl");
            crawl_with_retry_and_dlq(connector, storage, retry, source_id).await;
        })
    })
    // Include `cron_utc` in the error context so an
    // operator sees the offending expression immediately
    // — the loader deliberately doesn't pre-validate cron
    // syntax (scheduler is the authority), so this is
    // the first surface where a typo lands.
    .with_context(|| {
        format!("failed to construct cron job for {source_id} (cron_utc = {cron_utc_for_log:?})")
    })?;
    scheduler
        .add(job)
        .await
        .with_context(|| format!("failed to register cron job for {source_id}"))?;
    tracing::info!(
        source = %source_id,
        cron_utc = %cron_utc_for_log,
        retry_max_attempts = retry.max_attempts,
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
        // Log the bounded form — `ConnectorError::BadStatus`'s
        // Display carries the full upstream body, which is the
        // same risk we already capped for the DLQ row. Keep the
        // log line and the DLQ row symmetric on size.
        tracing::error!(
            source = %source_id,
            attempts,
            error_kind = dlq_error_kind(&error),
            error = %log_friendly(&error),
            "crawl pass failed after retries; writing to etl_dlq",
        );
        let error_kind = connector_error_to_dlq_kind(&error);
        // `crawl_pass` reflects the actual unit of work
        // (the whole `run_one_pass` — drain pagination,
        // domain-resolve each dataset, upsert into PG)
        // rather than naming a single phase like
        // `list_datasets`. As future ETL surfaces grow
        // their own retry envelopes (per-dataset
        // `fetch_data`, ETag refresh, etc.), each picks
        // its own `job_kind` so the DLQ filters target a
        // specific failure mode.
        //
        // `error_message`'s prefix is capped at
        // `DLQ_MESSAGE_CHAR_LIMIT` (Unicode scalars, not
        // UTF-16 code units) — when truncation fires the
        // stored value carries an extra `…[truncated]`
        // marker so an operator can tell the row hit the
        // cap. The full upstream payload (status code +
        // body excerpt) lives in `payload` for the
        // `BadStatus` variant; other variants put `None`
        // there since their `Display` impl already
        // carries the diagnostic.
        let (error_message, payload) = build_dlq_message_and_payload(&error);
        // `attempts` is a `u32` from the envelope, but the
        // loader caps `retry_max_attempts` at 1000 (way
        // below `i32::MAX`), so this conversion is
        // infallible. `expect` makes the invariant
        // explicit — if it ever fires, the cap drifted
        // and the operator's misconfig has reached this
        // code path.
        let entry = NewDlqEntry {
            source: source_id.as_str().to_string(),
            job_kind: "crawl_pass".to_string(),
            attempts: i32::try_from(attempts)
                .expect("attempts ≤ MAX_RETRY_ATTEMPTS (boundary-validated)"),
            error_kind,
            error_message,
            payload,
        };
        if let Err(e) = EtlDlqRepo::insert(&storage, entry).await {
            tracing::error!(error = %e, "could not write DLQ row");
        }
    }
}

/// Truncate `s` so the **prefix** carries at most
/// `limit` Unicode scalars. When truncation fires, an
/// ellipsis marker (`…[truncated]`) is appended to the
/// prefix so an operator reading the DLQ can tell the
/// row hit the cap — meaning the returned string is at
/// most `limit + len("…[truncated]")` scalars when
/// truncated, and exactly `s` (≤ `limit` scalars)
/// otherwise. Unicode-scalar counting matches the M5a
/// comment-body cap so CJK text doesn't surprise an
/// operator who configured the limit in zh-TW
/// characters.
///
/// Single-pass: walks the iterator up to `limit + 1`
/// chars, no more. A naive `chars().count() <= limit` check
/// would scan the whole input before deciding whether to
/// truncate — defeating the cap's purpose when an upstream
/// returns megabytes. Here the work is bounded by `limit`
/// regardless of input size.
fn truncate_scalars(s: &str, limit: usize) -> String {
    let mut iter = s.chars();
    let mut out = String::new();
    for _ in 0..limit {
        match iter.next() {
            Some(c) => out.push(c),
            // String was ≤ limit chars; return what we
            // accumulated, no marker needed.
            None => return out,
        }
    }
    // We consumed `limit` chars. If at least one more
    // remains, truncation happened — peek once and emit
    // the marker accordingly.
    if iter.next().is_some() {
        out.push_str("…[truncated]");
    }
    out
}

/// Direct `ConnectorError → DlqErrorKind` mapping.
///
/// The alternative would be a string round-trip via
/// `dlq_error_kind(&err)` then `DlqErrorKind::from_wire(...)`
/// then `unwrap_or(Other)`, which silently absorbs any
/// future drift between `ConnectorError` variants and
/// `DlqErrorKind` rows. With this direct exhaustive match,
/// adding a new `ConnectorError` variant fails to compile
/// until a matching `DlqErrorKind` row, CHECK, and
/// `from_wire` case land — exactly the lockstep the type
/// system can enforce. `dlq_error_kind` (returning
/// `&'static str`) stays in `retry.rs` for tracing-log use.
fn connector_error_to_dlq_kind(err: &connectors::ConnectorError) -> DlqErrorKind {
    use connectors::ConnectorError as CE;
    match err {
        CE::Transport(_) => DlqErrorKind::Transport,
        CE::BadStatus { .. } => DlqErrorKind::BadStatus,
        CE::Decode(_) => DlqErrorKind::Decode,
        CE::Config(_) => DlqErrorKind::Config,
        CE::InvalidCursor { .. } => DlqErrorKind::InvalidCursor,
        CE::Unsupported(_) => DlqErrorKind::Unsupported,
    }
}

/// Split a `ConnectorError` into the primary message (capped) and
/// an optional structured payload. `BadStatus` is the only variant
/// that carries a separate body worth preserving; the others fold
/// the diagnostic into `Display` already.
fn build_dlq_message_and_payload(
    error: &connectors::ConnectorError,
) -> (String, Option<serde_json::Value>) {
    use connectors::ConnectorError;
    match error {
        ConnectorError::BadStatus { status, body } => (
            // Primary message is the compact "HTTP NNN" form —
            // the full body lives in the payload below.
            format!("HTTP {status}"),
            Some(json!({
                "status": *status,
                "body_excerpt": truncate_scalars(body, DLQ_PAYLOAD_BODY_CHAR_LIMIT),
            })),
        ),
        other => (
            truncate_scalars(&format!("{other}"), DLQ_MESSAGE_CHAR_LIMIT),
            None,
        ),
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

    #[test]
    fn truncate_scalars_passes_short_strings_through() {
        assert_eq!(truncate_scalars("hello", 10), "hello");
        assert_eq!(truncate_scalars("", 10), "");
    }

    #[test]
    fn truncate_scalars_caps_long_ascii() {
        let s = "a".repeat(100);
        let out = truncate_scalars(&s, 20);
        // 20 'a's + the truncation marker. Marker is
        // counted separately so the operator can tell the
        // row hit the cap.
        assert!(out.starts_with(&"a".repeat(20)));
        assert!(out.ends_with("…[truncated]"));
    }

    #[test]
    fn truncate_scalars_counts_unicode_scalars_not_bytes() {
        // 5 CJK scalars × 3 bytes each = 15 bytes; cap at
        // 3 scalars truncates after the third char, not
        // mid-byte.
        let s = "資料庫錯誤";
        let out = truncate_scalars(s, 3);
        assert!(
            out.starts_with("資料庫"),
            "expected first 3 scalars preserved, got {out:?}",
        );
        assert!(out.ends_with("…[truncated]"));
    }

    #[test]
    fn truncate_scalars_at_exact_limit_does_not_mark() {
        // Inputs at exactly `limit` scalars are NOT
        // truncated (we never read a (limit+1)th char,
        // so no marker is emitted). Pins the boundary
        // behaviour against off-by-one regressions.
        let s = "a".repeat(20);
        let out = truncate_scalars(&s, 20);
        assert_eq!(out, s);
        assert!(!out.contains("truncated"));
    }

    #[test]
    fn build_dlq_for_bad_status_carries_structured_payload() {
        let huge_body = "x".repeat(10_000);
        let err = connectors::ConnectorError::BadStatus {
            status: 502,
            body: huge_body,
        };
        let (msg, payload) = build_dlq_message_and_payload(&err);
        // Primary message is compact — no body.
        assert_eq!(msg, "HTTP 502");
        let payload = payload.expect("BadStatus should produce payload");
        assert_eq!(payload["status"], 502);
        let excerpt = payload["body_excerpt"].as_str().expect("body_excerpt str");
        // Excerpt capped at DLQ_PAYLOAD_BODY_CHAR_LIMIT
        // plus the truncation marker.
        assert!(
            excerpt.chars().count() <= DLQ_PAYLOAD_BODY_CHAR_LIMIT + "…[truncated]".chars().count(),
            "excerpt too long: {} scalars",
            excerpt.chars().count(),
        );
        assert!(excerpt.ends_with("…[truncated]"));
    }

    #[test]
    fn build_dlq_for_other_errors_uses_display_capped() {
        let err = connectors::ConnectorError::Decode("schema drift on field foo".into());
        let (msg, payload) = build_dlq_message_and_payload(&err);
        assert!(msg.contains("schema drift on field foo"));
        assert!(payload.is_none(), "non-BadStatus should have no payload");
    }

    /// `build_connector_for` is the choke point that gates an
    /// `enabled = true` source against an actually-implemented
    /// connector. Today `DataGovTw` + `Twse` are
    /// implemented; the others must error loudly so a
    /// sources.toml typo or a premature flip can't
    /// silently drop a crawl.
    ///
    /// `Twse` isn't asserted here because its construction
    /// fetches a real `robots.txt` from `www.twse.com.tw`,
    /// which a unit test shouldn't depend on. The TWSE
    /// builder's `auto_fetch_robots(false)` escape hatch
    /// covers that surface in `connectors::twse::tests`.
    #[tokio::test]
    async fn build_connector_implemented_sources_succeed() {
        let path = "config/sources.toml";
        assert!(build_connector_for(SourceId::DataGovTw, path).await.is_ok());
        // M5b.3-5 will turn these into `Ok` as their connectors
        // land; flipping the assertion in lockstep keeps the
        // test the spec.
        assert!(build_connector_for(SourceId::Moea, path).await.is_err());
        assert!(build_connector_for(SourceId::Cwa, path).await.is_err());
        assert!(
            build_connector_for(SourceId::FisheryMoa, path)
                .await
                .is_err()
        );
        // user_contrib is never ETL-driven.
        assert!(
            build_connector_for(SourceId::UserContrib, path)
                .await
                .is_err()
        );
    }

    /// The error message must surface the actual loaded path,
    /// not the default — otherwise an operator using
    /// `SOURCES_CONFIG_PATH` to point at e.g. `/etc/td-hub/sources.toml`
    /// would be told to edit the wrong file. Uses `Moea`
    /// (still unimplemented in M5b.2) to exercise the
    /// path-aware error.
    #[tokio::test]
    async fn build_connector_error_message_carries_custom_path() {
        let path = "/etc/td-hub/sources.toml";
        let result = build_connector_for(SourceId::Moea, path).await;
        // Can't use `unwrap_err` because the Ok variant
        // (`Arc<dyn SourceConnector>`) doesn't implement
        // `Debug`. `let-else` keeps clippy's
        // `manual-let-else` lint happy.
        let Err(err) = result else {
            panic!("expected Moea to be unimplemented")
        };
        let msg = format!("{err}");
        assert!(
            msg.contains(path),
            "expected {path:?} in message, got {msg:?}"
        );
    }
}
