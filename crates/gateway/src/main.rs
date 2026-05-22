//! HTTP + MCP gateway (Axum + tower) for Taiwan Data Hub.
//!
//! Serves liveness/readiness probes plus the MCP 2025-11-25 Streamable HTTP
//! transport at `/mcp` (POST for client→server JSON-RPC, GET for the
//! backward-compat SSE upgrade). The MCP dispatcher is shared with the
//! stdio shim: both transports talk to the same `mcp_core::McpServer`, so
//! tools register once and reach every client.
//!
//! Two subcommands are exposed via clap (#4.1):
//!
//! - `serve` (default) — long-running HTTP listener
//! - `doctor` — parses every env knob the binary reads, reports what
//!   would and would not be wired, and exits non-zero on a hard config
//!   error so operators can catch typos before a redeploy.

mod api_keys_routes;
mod session_middleware;

use std::net::SocketAddr;

use anyhow::Context;
use axum::http::{HeaderName, Method, header};
use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::get};
use clap::{Parser, Subcommand};
use mcp_core::rmcp::model::Implementation;
use mcp_core::rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use mcp_core::{Dispatcher, DispatcherBuilder, McpServer};
use object_store::{LocalFsObjectStore, ObjectStore, S3Credentials, S3ObjectStore};
use serde::Serialize;
use shared::{Mode, ModeParseError};
use std::sync::Arc;
use storage::Storage;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tools_data::ObjectStoreRouter;
use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;
use tracing::info;
use tracing_subscriber::EnvFilter;
use url::Url;

const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
const GIT_SHA: &str = env!("GATEWAY_GIT_SHA");

/// Default listening address. Overridden by the `GATEWAY_ADDR` env var.
const DEFAULT_ADDR: &str = "0.0.0.0:8080";

/// Custom header per the MCP 2025-11-25 Streamable HTTP spec — used to bind
/// a session across multiple HTTP requests.
const MCP_SESSION_ID: HeaderName = HeaderName::from_static("mcp-session-id");

/// HTTP routes the gateway always exposes without auth. The `serve`
/// boot log prints this list so operators can sanity-check what is
/// reachable; the `doctor` subcommand reuses the same source of truth
/// so the two outputs cannot disagree.
const PUBLIC_ROUTES: &[&str] = &["/healthz", "/readyz", "/mcp"];

#[derive(Parser, Debug)]
#[command(
    version = PKG_VERSION,
    about = "Taiwan Data Hub HTTP + MCP gateway",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the long-lived HTTP + MCP gateway (default if no
    /// subcommand is given).
    Serve,
    /// Validate every env var the gateway reads and print a
    /// human-readable report. Exits 0 when the config is consistent,
    /// 1 when a hard error is detected.
    Doctor,
}

#[derive(Serialize)]
struct HealthBody {
    status: &'static str,
    version: &'static str,
    build_sha: &'static str,
}

#[derive(Serialize)]
struct ReadyBody {
    status: &'static str,
    version: &'static str,
    build_sha: &'static str,
    /// Per-check booleans so operators can see which dependency is failing.
    checks: ReadyChecks,
}

#[derive(Serialize)]
struct ReadyChecks {
    /// `Some(true)` when the PG pool is reachable AND migration version
    /// matches; `Some(false)` when configured but unreachable;
    /// `None` while `DATABASE_URL` is unset (which is the case until M0
    /// #0.3 + #0.8 wire up the pool).
    database: Option<bool>,
}

/// Liveness probe — always 200. Kubelet uses this to decide whether
/// to restart the container, not to gate traffic.
async fn healthz() -> impl IntoResponse {
    Json(HealthBody {
        status: "ok",
        version: PKG_VERSION,
        build_sha: GIT_SHA,
    })
}

/// Readiness probe — 200 when every dependency is reachable, 503
/// otherwise so load balancers stop sending traffic. The database
/// check is stubbed until M0 #0.3 + #0.8 wire up the real sqlx pool —
/// see `dependency_ready`.
async fn readyz() -> impl IntoResponse {
    let database = dependency_ready(non_empty_env("DATABASE_URL").as_deref());
    let all_ready = database.unwrap_or(false);

    let body = ReadyBody {
        status: if all_ready { "ready" } else { "not_ready" },
        version: PKG_VERSION,
        build_sha: GIT_SHA,
        checks: ReadyChecks { database },
    };

    let status = if all_ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (status, Json(body))
}

/// Reports whether the database dependency is ready, given the value
/// (if any) of `DATABASE_URL`. Until M0 #0.3 (compose with `PostgreSQL`
/// 18) and M0 #0.8 (initial sqlx migrations) land, the URL is unset and
/// we return `None` ("not configured"), which collapses to a 503 from
/// /readyz. The real sqlx pool ping lands when the dependency does.
///
/// Pure (no env access) so it can be unit-tested without the
/// `unsafe`-marked `std::env::set_var` API.
fn dependency_ready(database_url: Option<&str>) -> Option<bool> {
    database_url.map(|_| {
        // TODO(#0.8): replace with `pool.acquire().await.is_ok()` plus
        // a `SELECT MAX(version) FROM _sqlx_migrations` check.
        false
    })
}

/// Build the CORS layer for `/mcp`. Allows any origin for the moment so
/// local browser-based Inspector clients (e.g. `http://localhost:6274`)
/// can call into a gateway on a different port; M4 will tighten this when
/// auth lands. Methods and headers follow the Streamable HTTP spec; we
/// expose `Mcp-Session-Id` so clients can read the session id off the
/// initialize response.
fn build_mcp_cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(tower_http::cors::Any)
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE, header::ACCEPT, MCP_SESSION_ID])
        .expose_headers([MCP_SESSION_ID])
}

/// Build the MCP `Implementation` advertised in the initialize response.
/// Passed in explicitly because `Implementation::from_build_env()` resolves
/// at rmcp's compile site and would always report `name: "rmcp"`.
fn gateway_implementation() -> Implementation {
    Implementation::new(env!("CARGO_PKG_NAME"), PKG_VERSION)
}

fn build_router(
    server: McpServer,
    auth_router: Option<Router>,
    cancel: CancellationToken,
) -> Router {
    let mcp_service = StreamableHttpService::new(
        move || Ok(server.clone()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default().with_cancellation_token(cancel),
    );

    // Scope CORS to /mcp only — /healthz and /readyz are infrastructure
    // probes and should retain default (no-CORS) behavior.
    let mcp_with_cors = ServiceBuilder::new()
        .layer(build_mcp_cors())
        .service(mcp_service);

    let mut router = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .nest_service("/mcp", mcp_with_cors);
    if let Some(auth) = auth_router {
        router = router.merge(auth);
    }
    router
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve().await,
        Command::Doctor => doctor(),
    }
}

async fn serve() -> anyhow::Result<()> {
    let mode = Mode::from_env().context("invalid MODE env var")?;
    let addr = read_gateway_addr()?;

    info!(
        mode = mode.as_str(),
        public_routes = PUBLIC_ROUTES.join(","),
        gated_routes = gated_route_list(mode),
        "operating mode resolved"
    );

    // Single Storage handle reused by the DB-backed MCP tools AND the
    // #4.6 api-keys subrouter. Connecting once at boot means a transient
    // DB outage doesn't double the gateway's startup pressure on the
    // pool; both consumers share the same pool's connection lifecycle.
    let storage = connect_storage_if_available().await;

    // Single MCP server shared by every session — Dispatcher is Arc-backed
    // so clone() in the factory is cheap. Stdio and HTTP both feed off the
    // same `tools_data::register_data_tools` helper, so tools register in
    // one place and reach every transport.
    let mut builder: DispatcherBuilder = tools_data::register_data_tools(Dispatcher::builder());
    builder = tools_utility::register_utility_tools(builder);
    builder = wire_db_tools_if_available(builder, storage.clone());
    let dispatcher = builder.build();
    let tool_count = dispatcher.len();
    let server = McpServer::new(dispatcher, gateway_implementation())
        .with_instructions("Taiwan Data Hub MCP server.");

    let cancel = CancellationToken::new();
    let auth_router = build_auth_router_if_available(storage);
    let app = build_router(server, auth_router, cancel.child_token());

    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;

    info!(
        version = PKG_VERSION,
        build_sha = GIT_SHA,
        addr = %addr,
        tools = tool_count,
        "gateway listening (HTTP + MCP Streamable at /mcp)"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(cancel))
        .await
        .context("axum server crashed")?;

    Ok(())
}

/// Validates the gateway's env knobs and prints a human-readable
/// report. Connection probes are deliberately out of scope: doctor
/// must run usefully in CI and on a fresh laptop where Postgres /
/// `SeaweedFS` aren't reachable. What it catches:
///
/// - invalid `MODE` value
/// - malformed `GATEWAY_ADDR`
/// - malformed object-store URLs
/// - partially-configured S3 credentials (some set, some missing)
///
/// Exits 1 on any hard error so a CI smoke test can `--fail-fast`
/// before a redeploy.
fn doctor() -> anyhow::Result<()> {
    let report = DoctorReport::collect();
    println!("{report}");
    let err_count = report.error_count();
    if err_count > 0 {
        anyhow::bail!("doctor found {err_count} config error(s); see report above");
    }
    Ok(())
}

/// Open a [`Storage`] handle if `DATABASE_URL` is set and a pool
/// can be established. Returning `Option<Storage>` lets the
/// caller share one handle across both the DB-backed MCP tools
/// AND the #4.6 api-keys subrouter — instead of each consumer
/// opening its own pool, we open one and clone the cheap
/// Arc-backed handle for each surface.
///
/// Failure to connect downgrades to `None` (and a `warn!`) rather
/// than killing the gateway: `/healthz` and `/readyz` stay
/// independently useful for ops, and personal-mode installs
/// without Postgres still get a working MCP server with
/// `list_domains`.
async fn connect_storage_if_available() -> Option<Storage> {
    // Use the same blank==unset normalisation as `readyz` and the
    // doctor so all three observers report the same configuration
    // state instead of disagreeing on whether `DATABASE_URL=` (set
    // but empty) means "configured".
    let Some(url) = non_empty_env("DATABASE_URL") else {
        // Log explicitly here, not just in
        // `build_auth_router_if_available`'s "no Storage" branch
        // — the docstring there promises "the underlying reason
        // was logged at its own boundary", and that's only true
        // if this line fires. Personal-mode boots without
        // `DATABASE_URL` set deliberately, so `info!` (not
        // `warn!`) is the right level.
        tracing::info!(
            "DATABASE_URL unset; DB-backed tools + api-keys subrouter disabled \
             (list_domains and other in-process tools still work)"
        );
        return None;
    };
    match Storage::connect(&url).await {
        Ok(storage) => {
            // Logged narrowly: only the DB-tools side is enabled
            // unconditionally once Storage opens. The api-keys
            // subrouter has a second gate (`SESSION_HMAC_KEY`)
            // that this layer can't see; let
            // `build_auth_router_if_available` log its own
            // enable / disable line so the boot log never claims
            // api-keys is wired up when it isn't.
            tracing::info!("DATABASE_URL connected; DB-backed tools enabled");
            Some(storage)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "DATABASE_URL set but Storage::connect failed; DB tools + api-keys disabled"
            );
            None
        }
    }
}

/// Register Postgres-backed tools onto the dispatcher when a
/// [`Storage`] handle is available. `None` is a hard no-op:
/// [`connect_storage_if_available`] already emitted the
/// canonical `DATABASE_URL unset / connect failed` log line
/// before returning `None`, so adding a second message here
/// would just double-log the same outcome on every boot
/// without auth wired up.
fn wire_db_tools_if_available(
    builder: DispatcherBuilder,
    storage: Option<Storage>,
) -> DispatcherBuilder {
    let Some(storage) = storage else {
        return builder;
    };
    let router = build_object_store_router();
    tools_data::register_db_tools(builder, storage, router)
}

/// Build the `/v1/api-keys` subrouter (session-gated). Returns
/// `None` when any required dependency is missing. The log level
/// distinguishes "intentionally disabled" from "misconfigured":
///
/// - `info!` for the EXPECTED disabled states — `storage` is
///   `None` because `DATABASE_URL` was deliberately unset, or
///   `SESSION_HMAC_KEY` is also deliberately unset (personal-
///   mode / dev). Operators running without auth see one
///   informational line per boot and that's it.
/// - `warn!` for MISCONFIGURATION — `SESSION_HMAC_KEY` was set
///   but the bytes are invalid base64 or shorter than the auth
///   crate's required minimum, or `SessionService::new` rejected
///   the key. These need operator attention because the
///   subrouter SHOULD have come up.
///
/// In either case the rest of the gateway still serves — the
/// account page just can't load. This matches the broader
/// "fail-soft on optional DB dependencies" posture of the binary.
fn build_auth_router_if_available(storage: Option<Storage>) -> Option<Router> {
    let Some(storage) = storage else {
        // `connect_storage_if_available` already logged the
        // underlying reason (URL unset or pool open failed) at
        // its own boundary. Repeat it here at `info!` so the
        // boot log has a single line that names the
        // api-keys subrouter explicitly — operators searching
        // for "api-keys" in the logs see one canonical
        // disabled-reason instead of having to cross-reference
        // two separate messages.
        tracing::info!("api-keys subrouter disabled: no Storage handle");
        return None;
    };
    let hmac_key = match auth_hmac_key_from_env() {
        Ok(Some(k)) => k,
        Ok(None) => {
            tracing::info!(
                "SESSION_HMAC_KEY unset; api-keys subrouter disabled (login + account UI inactive)"
            );
            return None;
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "SESSION_HMAC_KEY rejected; api-keys subrouter disabled"
            );
            return None;
        }
    };

    let session_repo: Arc<dyn storage::SessionRepo> = Arc::new(storage.clone());
    let api_key_repo: Arc<dyn storage::ApiKeyRepo> = Arc::new(storage);
    let session_service = match auth::SessionService::new(session_repo, hmac_key) {
        Ok(svc) => Arc::new(svc),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "SessionService::new failed; api-keys subrouter disabled"
            );
            return None;
        }
    };
    let api_key_service = Arc::new(auth::ApiKeyService::new(api_key_repo));

    // Mount the api-keys handlers behind the session middleware so
    // every handler receives `Option<Extension<ValidatedSession>>`
    // and can return 401 instead of letting axum 500 on a missing
    // extractor when the cookie was missing / expired / revoked.
    let router =
        api_keys_routes::router(api_key_service).layer(axum::middleware::from_fn_with_state(
            session_service,
            session_middleware::session_middleware,
        ));
    // Positive enable log — counterpart to the four
    // disabled-reason lines above. Operators grepping for
    // "api-keys" in the boot log see exactly one of these
    // five outcomes.
    tracing::info!("api-keys subrouter enabled at /v1/api-keys");
    Some(Router::new().nest("/v1/api-keys", router))
}

/// Decode `SESSION_HMAC_KEY` from base64. Returns `Ok(None)` when
/// the env var is unset or blank (so the gateway can fall back to
/// "auth disabled"), `Err(...)` when the var is set but invalid
/// (so operators see a typo at boot instead of a silent fallback).
fn auth_hmac_key_from_env() -> Result<Option<Vec<u8>>, String> {
    use base64::Engine;

    let Some(encoded) = non_empty_env("SESSION_HMAC_KEY") else {
        return Ok(None);
    };
    base64::engine::general_purpose::STANDARD
        .decode(encoded.as_bytes())
        .map(Some)
        .map_err(|e| format!("SESSION_HMAC_KEY is not valid base64: {e}"))
}

/// Assemble the `ObjectStoreRouter` from environment variables.
///
/// Both backends are independently optional — the gateway can run
/// without either (the tool will then surface a "no backend
/// configured" error) and personal-mode installs without S3 set up
/// only the local-FS backend.
///
/// Env knobs:
///
/// - `OBJECT_STORE_BASE_URL` + `OBJECT_STORE_SIGNING_SECRET` →
///   `LocalFsObjectStore` for `file://` URIs.
/// - `S3_ENDPOINT` + `S3_REGION` + `S3_ACCESS_KEY_ID` +
///   `S3_SECRET_ACCESS_KEY` (+ optional `S3_SESSION_TOKEN`) →
///   `S3ObjectStore` for `s3://` URIs.
fn build_object_store_router() -> ObjectStoreRouter {
    let mut router = ObjectStoreRouter::new();

    if let (Some(base), Some(secret)) = (
        non_empty_env("OBJECT_STORE_BASE_URL"),
        non_empty_env("OBJECT_STORE_SIGNING_SECRET"),
    ) {
        match Url::parse(&base) {
            Ok(base_url) => match LocalFsObjectStore::new(base_url, secret.into_bytes()) {
                Ok(store) => {
                    tracing::info!("local-fs object store wired (file:// URIs)");
                    router = router.with_local_fs(Arc::new(store) as Arc<dyn ObjectStore>);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "OBJECT_STORE_* set but LocalFs init failed");
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "OBJECT_STORE_BASE_URL is not a valid URL");
            }
        }
    }

    if let (Some(endpoint), Some(region), Some(key), Some(secret)) = (
        non_empty_env("S3_ENDPOINT"),
        non_empty_env("S3_REGION"),
        non_empty_env("S3_ACCESS_KEY_ID"),
        non_empty_env("S3_SECRET_ACCESS_KEY"),
    ) {
        match Url::parse(&endpoint) {
            Ok(endpoint_url) => {
                let creds = S3Credentials {
                    access_key_id: key,
                    secret_access_key: secret,
                    session_token: non_empty_env("S3_SESSION_TOKEN"),
                };
                match S3ObjectStore::new(endpoint_url, region, creds) {
                    Ok(store) => {
                        tracing::info!("s3 object store wired (s3:// URIs)");
                        router = router.with_s3(Arc::new(store) as Arc<dyn ObjectStore>);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "S3_* set but S3ObjectStore init failed");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "S3_ENDPOINT is not a valid URL");
            }
        }
    }

    router
}

/// Read an env var, trim whitespace, and return `Some(value)` only
/// when the result is non-empty. An empty string from
/// `std::env::var` (set but blank) would otherwise be wired into the
/// store as if it were a real credential — producing unusable
/// signatures and a misleading "wired" log line at boot. We treat
/// "set but blank" the same as "unset".
fn non_empty_env(key: &str) -> Option<String> {
    let raw = std::env::var(key).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        tracing::warn!(env = key, "environment variable is set but blank; ignoring");
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Parse `GATEWAY_ADDR` into a `SocketAddr` (falling back to
/// [`DEFAULT_ADDR`] when unset). Pure so `serve` and `doctor`
/// share the same parse + error-wording path: `serve` calls
/// [`read_gateway_addr`] which reads env and delegates here,
/// `doctor` calls [`validate_gateway_addr`] which delegates here
/// with the env value it already pulled into [`EnvSnapshot`].
fn read_gateway_addr_value(raw: Option<&str>) -> anyhow::Result<SocketAddr> {
    let raw = raw.unwrap_or(DEFAULT_ADDR);
    raw.parse().with_context(|| {
        format!("{raw:?}: GATEWAY_ADDR must be a valid socket address (host:port)")
    })
}

/// Read `GATEWAY_ADDR` from process env (or the default) and parse
/// it. Thin wrapper over [`read_gateway_addr_value`] for `serve`.
fn read_gateway_addr() -> anyhow::Result<SocketAddr> {
    read_gateway_addr_value(std::env::var("GATEWAY_ADDR").ok().as_deref())
}

/// Returns a render-ready string describing which routes require
/// auth in the given mode. Auth middleware lands in #4.5 — until
/// then both modes serve every route publicly, so this returns
/// `"<none>"` for `Personal` and `"<pending #4.5>"` for `MultiUser`.
/// Callers (`serve`'s boot log and `doctor`'s report) embed the
/// returned string verbatim so the two outputs always agree.
fn gated_route_list(mode: Mode) -> &'static str {
    match mode {
        Mode::Personal => "<none>",
        Mode::MultiUser => "<pending #4.5>",
    }
}

/// Returns when the process receives SIGINT or SIGTERM. Cancels the shared
/// MCP cancellation token first so any in-flight `/mcp` sessions abort
/// cleanly, then lets axum drain HTTP requests.
async fn shutdown_signal(cancel: CancellationToken) {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");

    tokio::select! {
        _ = sigterm.recv() => info!("received SIGTERM, shutting down"),
        _ = sigint.recv() => info!("received SIGINT, shutting down"),
    }

    cancel.cancel();
}

// -------- doctor report ------------------------------------------------

/// Findings from one `doctor` run. Held as data (not printed
/// inline) so the report struct can be unit-tested without
/// capturing stdout.
#[derive(Debug, Default)]
struct DoctorReport {
    entries: Vec<DoctorEntry>,
}

#[derive(Debug, Clone)]
struct DoctorEntry {
    label: String,
    status: DoctorStatus,
    detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoctorStatus {
    /// Config is valid and ready to use — either set explicitly or
    /// resolved from a documented default (e.g. [`DEFAULT_ADDR`]).
    Ok,
    /// Config is absent but the binary tolerates that (e.g. no
    /// `DATABASE_URL` in a personal-mode install). Always rendered
    /// `info`, never an error.
    Info,
    /// Config is present but malformed, partial, or contradictory.
    /// Counts toward [`DoctorReport::error_count`] so the process
    /// exits non-zero.
    Error,
}

impl DoctorReport {
    fn collect() -> Self {
        let env = EnvSnapshot::from_process_env();
        Self::from_snapshot(&env)
    }

    fn from_snapshot(env: &EnvSnapshot) -> Self {
        let mut report = Self::default();
        report.push_entry(validate_mode(&env.mode));
        report.push_entry(validate_gateway_addr(env.gateway_addr.as_deref()));
        report.push_entry(validate_database_url(env.database_url.as_deref()));
        report.push_entry(validate_object_store(
            env.object_store_base_url.as_deref(),
            env.object_store_signing_secret.as_deref(),
        ));
        report.push_entry(validate_s3(
            env.s3_endpoint.as_deref(),
            env.s3_region.as_deref(),
            env.s3_access_key_id.as_deref(),
            env.s3_secret_access_key.as_deref(),
            env.s3_session_token.as_deref(),
        ));
        report
    }

    fn error_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.status == DoctorStatus::Error)
            .count()
    }

    #[cfg(test)]
    fn has_errors(&self) -> bool {
        self.error_count() > 0
    }

    #[cfg(test)]
    fn push(&mut self, label: &str, status: DoctorStatus, detail: impl Into<String>) {
        self.push_entry(DoctorEntry {
            label: label.to_owned(),
            status,
            detail: detail.into(),
        });
    }

    fn push_entry(&mut self, entry: DoctorEntry) {
        self.entries.push(entry);
    }
}

/// Snapshot of every env var the doctor inspects. Held as data so
/// the pure validators below can be unit-tested without touching
/// process env (which would require the forbidden
/// `unsafe { std::env::set_var }`).
#[derive(Debug, Default, Clone)]
struct EnvSnapshot {
    mode: ModeRaw,
    gateway_addr: Option<String>,
    database_url: Option<String>,
    object_store_base_url: Option<String>,
    object_store_signing_secret: Option<String>,
    s3_endpoint: Option<String>,
    s3_region: Option<String>,
    s3_access_key_id: Option<String>,
    s3_secret_access_key: Option<String>,
    s3_session_token: Option<String>,
}

impl EnvSnapshot {
    fn from_process_env() -> Self {
        Self {
            mode: ModeRaw::from_process_env(),
            gateway_addr: std::env::var("GATEWAY_ADDR").ok(),
            database_url: non_empty_env("DATABASE_URL"),
            object_store_base_url: non_empty_env("OBJECT_STORE_BASE_URL"),
            object_store_signing_secret: non_empty_env("OBJECT_STORE_SIGNING_SECRET"),
            s3_endpoint: non_empty_env("S3_ENDPOINT"),
            s3_region: non_empty_env("S3_REGION"),
            s3_access_key_id: non_empty_env("S3_ACCESS_KEY_ID"),
            s3_secret_access_key: non_empty_env("S3_SECRET_ACCESS_KEY"),
            s3_session_token: non_empty_env("S3_SESSION_TOKEN"),
        }
    }
}

/// Tri-state for `MODE`: unset, set to a UTF-8 value, or set to bytes
/// that are not valid UTF-8. Modeled explicitly so doctor surfaces
/// the non-Unicode case as an error instead of silently collapsing
/// it to "unset" — which would let doctor say `[ok] personal` while
/// `serve` aborts with `ModeParseError::NonUnicode`.
#[derive(Debug, Clone, Default)]
enum ModeRaw {
    #[default]
    Unset,
    Set(String),
    NonUnicode,
}

impl ModeRaw {
    fn from_process_env() -> Self {
        match std::env::var(shared::MODE_ENV) {
            Ok(s) => Self::Set(s),
            Err(std::env::VarError::NotPresent) => Self::Unset,
            Err(std::env::VarError::NotUnicode(_)) => Self::NonUnicode,
        }
    }
}

fn doctor_entry(label: &str, status: DoctorStatus, detail: impl Into<String>) -> DoctorEntry {
    DoctorEntry {
        label: label.to_owned(),
        status,
        detail: detail.into(),
    }
}

fn validate_mode(raw: &ModeRaw) -> DoctorEntry {
    let parsed = match raw {
        ModeRaw::NonUnicode => Err(ModeParseError::NonUnicode),
        ModeRaw::Unset => Mode::from_env_value(None),
        ModeRaw::Set(s) => Mode::from_env_value(Some(s.as_str())),
    };
    match parsed {
        Ok(mode) => doctor_entry(
            "MODE",
            DoctorStatus::Ok,
            format!(
                "{mode} (public: {}; gated: {})",
                PUBLIC_ROUTES.join(","),
                gated_route_list(mode),
            ),
        ),
        Err(e) => doctor_entry("MODE", DoctorStatus::Error, e.to_string()),
    }
}

fn validate_gateway_addr(raw: Option<&str>) -> DoctorEntry {
    match read_gateway_addr_value(raw) {
        Ok(addr) => doctor_entry("GATEWAY_ADDR", DoctorStatus::Ok, addr.to_string()),
        Err(e) => doctor_entry("GATEWAY_ADDR", DoctorStatus::Error, format!("{e:#}")),
    }
}

fn validate_database_url(raw: Option<&str>) -> DoctorEntry {
    match raw {
        None => doctor_entry(
            "DATABASE_URL",
            DoctorStatus::Info,
            "unset — DB-backed tools will be disabled",
        ),
        Some(raw) => match Url::parse(raw) {
            Err(e) => doctor_entry(
                "DATABASE_URL",
                DoctorStatus::Error,
                format!("not a valid URL: {e}"),
            ),
            Ok(url) => match url.scheme() {
                "postgres" | "postgresql" => doctor_entry(
                    "DATABASE_URL",
                    DoctorStatus::Ok,
                    "<set> (connection probe skipped; run `cargo test --release -p storage -- --ignored` for end-to-end)",
                ),
                other => doctor_entry(
                    "DATABASE_URL",
                    DoctorStatus::Error,
                    format!(
                        "scheme {other:?} is not supported by sqlx::PgPool; expected `postgres` or `postgresql`"
                    ),
                ),
            },
        },
    }
}

fn validate_object_store(base: Option<&str>, secret: Option<&str>) -> DoctorEntry {
    match (base, secret) {
        (None, None) => doctor_entry(
            "OBJECT_STORE_*",
            DoctorStatus::Info,
            "unset — file:// URIs unavailable",
        ),
        (Some(_), None) | (None, Some(_)) => doctor_entry(
            "OBJECT_STORE_*",
            DoctorStatus::Error,
            "partial config: both OBJECT_STORE_BASE_URL and OBJECT_STORE_SIGNING_SECRET must be set together",
        ),
        (Some(b), Some(s)) => match Url::parse(b) {
            Err(e) => doctor_entry(
                "OBJECT_STORE_BASE_URL",
                DoctorStatus::Error,
                format!("not a valid URL: {e}"),
            ),
            // Mirror runtime wiring exactly: a syntactically-valid URL
            // can still be rejected by `LocalFsObjectStore::new` for
            // origin/secret-length reasons. Construct and discard so
            // doctor catches what the runtime would warn-and-skip.
            Ok(url) => match LocalFsObjectStore::new(url, s.as_bytes().to_vec()) {
                Ok(_) => doctor_entry("OBJECT_STORE_*", DoctorStatus::Ok, format!("base={b}")),
                Err(e) => doctor_entry(
                    "OBJECT_STORE_*",
                    DoctorStatus::Error,
                    format!("invalid config: {e}"),
                ),
            },
        },
    }
}

fn validate_s3(
    endpoint: Option<&str>,
    region: Option<&str>,
    key: Option<&str>,
    secret: Option<&str>,
    session_token: Option<&str>,
) -> DoctorEntry {
    let required = [endpoint, region, key, secret];
    let required_any = required.iter().any(Option::is_some);
    let required_all = required.iter().all(Option::is_some);
    let token_set = session_token.is_some();

    if !required_any && !token_set {
        return doctor_entry("S3_*", DoctorStatus::Info, "unset — s3:// URIs unavailable");
    }
    if !required_all {
        // Either a required field is missing, or only S3_SESSION_TOKEN
        // was set — both are partial configs the runtime can't wire.
        return doctor_entry(
            "S3_*",
            DoctorStatus::Error,
            "partial config: S3_ENDPOINT, S3_REGION, S3_ACCESS_KEY_ID, S3_SECRET_ACCESS_KEY must be set together (S3_SESSION_TOKEN is optional)",
        );
    }
    let endpoint = endpoint.expect("checked above");
    let region = region.expect("checked above");
    let key = key.expect("checked above");
    let secret = secret.expect("checked above");
    let token_note = if token_set {
        " (with session token)"
    } else {
        ""
    };
    match Url::parse(endpoint) {
        Err(e) => doctor_entry(
            "S3_ENDPOINT",
            DoctorStatus::Error,
            format!("not a valid URL: {e}"),
        ),
        // Mirror runtime wiring: `S3ObjectStore::new` rejects endpoints
        // that carry a path/query/fragment because the path is later
        // overwritten with `/{bucket}/{key}` and any prefix would
        // silently disappear from the signature. Construct and discard
        // so doctor catches the same misconfig the runtime warn-skips.
        Ok(url) => {
            let creds = S3Credentials {
                access_key_id: key.to_owned(),
                secret_access_key: secret.to_owned(),
                session_token: session_token.map(str::to_owned),
            };
            match S3ObjectStore::new(url, region.to_owned(), creds) {
                Ok(_) => doctor_entry(
                    "S3_*",
                    DoctorStatus::Ok,
                    format!("endpoint={endpoint}{token_note}"),
                ),
                Err(e) => doctor_entry("S3_*", DoctorStatus::Error, format!("invalid config: {e}")),
            }
        }
    }
}

impl std::fmt::Display for DoctorReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Use CARGO_PKG_NAME so the banner matches what clap prints
        // for `--help` (and what operators actually invoked).
        writeln!(
            f,
            "{bin} doctor (v{PKG_VERSION})",
            bin = env!("CARGO_PKG_NAME"),
        )?;
        writeln!(f, "build_sha={GIT_SHA}")?;
        writeln!(f, "---")?;
        for entry in &self.entries {
            let marker = match entry.status {
                DoctorStatus::Ok => "[ok]",
                DoctorStatus::Info => "[info]",
                DoctorStatus::Error => "[error]",
            };
            writeln!(f, "{marker} {}: {}", entry.label, entry.detail)?;
        }
        let err_count = self.error_count();
        writeln!(f, "---")?;
        if err_count == 0 {
            writeln!(f, "all checks passed")
        } else {
            writeln!(f, "{err_count} error(s) — fix and re-run")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_server() -> McpServer {
        McpServer::new(
            Dispatcher::default(),
            Implementation::new("gateway-test", "0.0.1"),
        )
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn healthz_returns_200_with_version_and_sha() {
        let app = build_router(test_server(), None, CancellationToken::new());
        let resp = app
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["status"], "ok");
        assert_eq!(json["version"], PKG_VERSION);
        assert!(json["build_sha"].is_string());
    }

    // The /readyz endpoint reads DATABASE_URL from the process env,
    // which would require `unsafe { std::env::set_var(...) }` from a
    // test. We forbid unsafe at the workspace level, so we test the
    // pure `dependency_ready` helper instead — it's what /readyz
    // actually delegates to.

    #[test]
    fn dependency_ready_is_none_when_database_url_unset() {
        assert_eq!(dependency_ready(None), None);
    }

    #[test]
    fn dependency_ready_is_some_false_until_real_pool_wired_up() {
        // Stub returns Some(false) for ANY non-empty URL. M0 #0.8 will
        // replace this with a real `pool.acquire()` check and this test
        // will move into a testcontainer-backed integration test.
        assert_eq!(dependency_ready(Some("postgres://stub/stub")), Some(false));
    }

    /// Regression for Copilot PR #91 round 1: CORS must apply to `/mcp`
    /// only, not the infrastructure probes. An OPTIONS preflight to
    /// `/healthz` should NOT come back with `access-control-allow-origin`,
    /// while the same preflight to `/mcp` should.
    #[tokio::test]
    async fn cors_is_scoped_to_mcp_and_does_not_leak_to_healthz() {
        let app = build_router(test_server(), None, CancellationToken::new());

        let mcp_preflight = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/mcp")
                    .header("host", "127.0.0.1:8080")
                    .header("origin", "http://example.invalid")
                    .header("access-control-request-method", "POST")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            mcp_preflight
                .headers()
                .contains_key("access-control-allow-origin"),
            "/mcp preflight should carry CORS headers",
        );

        let healthz_preflight = app
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/healthz")
                    .header("host", "127.0.0.1:8080")
                    .header("origin", "http://example.invalid")
                    .header("access-control-request-method", "GET")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            !healthz_preflight
                .headers()
                .contains_key("access-control-allow-origin"),
            "CORS must not leak onto /healthz",
        );
    }

    /// Streamable HTTP requires `Accept: application/json, text/event-stream`
    /// on POST /mcp. The initialize response is JSON (not SSE) and carries
    /// the negotiated protocol version + the server's identity.
    #[tokio::test]
    async fn mcp_post_initialize_returns_2025_11_25_with_gateway_identity() {
        let app = build_router(test_server(), None, CancellationToken::new());

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "gateway-test", "version": "0.0.1"},
            },
        });

        let resp = app
            .oneshot(
                Request::post("/mcp")
                    .header("host", "127.0.0.1:8080")
                    .header("content-type", "application/json")
                    .header("accept", "application/json, text/event-stream")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let session = resp.headers().get(&MCP_SESSION_ID).cloned();
        let content_type = resp
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap().to_owned());
        let bytes = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
        let raw = String::from_utf8_lossy(&bytes);

        // Per MCP 2025-11-25 the server picks application/json or
        // text/event-stream based on Accept. With both offered, rmcp
        // currently chooses SSE; the first frame is an empty
        // `data: \nid: 0\nretry: ...` keepalive, so skip empties.
        let json: serde_json::Value = match content_type.as_deref() {
            Some(ct) if ct.starts_with("text/event-stream") => {
                let data = raw
                    .lines()
                    .filter_map(|l| {
                        l.strip_prefix("data:")
                            .map(|s| s.strip_prefix(' ').unwrap_or(s))
                    })
                    .find(|s| !s.is_empty())
                    .unwrap_or_else(|| panic!("no non-empty SSE data frame in body:\n{raw}"));
                serde_json::from_str(data).unwrap_or_else(|e| {
                    panic!("SSE data frame is not JSON: {e}; data was: {data:?}")
                })
            }
            _ => serde_json::from_str(&raw)
                .unwrap_or_else(|e| panic!("response is not JSON: {e}; raw: {raw:?}")),
        };

        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["id"], 1);
        assert_eq!(json["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(json["result"]["serverInfo"]["name"], "gateway-test");
        assert!(
            session.is_some(),
            "Mcp-Session-Id must be set on the initialize response"
        );
    }

    #[test]
    fn gated_route_list_distinguishes_modes() {
        assert_eq!(gated_route_list(Mode::Personal), "<none>");
        assert_eq!(gated_route_list(Mode::MultiUser), "<pending #4.5>");
    }

    #[test]
    fn doctor_report_marks_partial_s3_as_error() {
        let mut report = DoctorReport::default();
        report.push("S3_*", DoctorStatus::Error, "partial config");
        assert!(report.has_errors());
    }

    #[test]
    fn doctor_report_with_only_ok_and_info_does_not_error() {
        let mut report = DoctorReport::default();
        report.push("MODE", DoctorStatus::Ok, "personal");
        report.push("DATABASE_URL", DoctorStatus::Info, "unset");
        assert!(!report.has_errors());
    }

    #[test]
    fn doctor_report_display_renders_markers_and_summary() {
        let mut report = DoctorReport::default();
        report.push("MODE", DoctorStatus::Ok, "personal");
        report.push("X", DoctorStatus::Info, "unset");
        report.push("Y", DoctorStatus::Error, "boom");
        let rendered = format!("{report}");
        assert!(rendered.contains("[ok] MODE: personal"));
        assert!(rendered.contains("[info] X: unset"));
        assert!(rendered.contains("[error] Y: boom"));
        assert!(rendered.contains("1 error"));
    }

    #[test]
    fn validate_mode_renders_resolved_routes() {
        let entry = validate_mode(&ModeRaw::Unset);
        assert_eq!(entry.status, DoctorStatus::Ok);
        assert!(entry.detail.starts_with("personal "));
        assert!(entry.detail.contains("public: /healthz,/readyz,/mcp"));
        assert!(entry.detail.contains("gated: <none>"));

        let entry = validate_mode(&ModeRaw::Set("multi-user".to_owned()));
        assert!(entry.detail.contains("gated: <pending #4.5>"));
    }

    #[test]
    fn validate_mode_flags_unknown_value() {
        let entry = validate_mode(&ModeRaw::Set("garbage".to_owned()));
        assert_eq!(entry.status, DoctorStatus::Error);
        assert!(entry.detail.contains("invalid MODE value"));
    }

    #[test]
    fn validate_mode_flags_non_unicode_env() {
        // The runtime errors on a non-Unicode MODE via Mode::from_env;
        // doctor must surface the same condition rather than collapse
        // it to "unset → personal".
        let entry = validate_mode(&ModeRaw::NonUnicode);
        assert_eq!(entry.status, DoctorStatus::Error);
        assert!(entry.detail.contains("not valid UTF-8"));
    }

    #[test]
    fn validate_gateway_addr_uses_default_when_unset() {
        let entry = validate_gateway_addr(None);
        assert_eq!(entry.status, DoctorStatus::Ok);
        assert_eq!(entry.detail, "0.0.0.0:8080");
    }

    #[test]
    fn validate_gateway_addr_rejects_malformed() {
        let entry = validate_gateway_addr(Some("not-an-addr"));
        assert_eq!(entry.status, DoctorStatus::Error);
        assert!(entry.detail.contains("\"not-an-addr\""));
    }

    /// A signing secret long enough to satisfy `LocalFsObjectStore`'s
    /// 32-byte minimum without leaking a real key into tests.
    const TEST_SIGNING_SECRET: &str = "test-only-signing-secret-32-bytes-min";

    #[test]
    fn validate_database_url_distinguishes_unset_set_and_bad() {
        assert_eq!(validate_database_url(None).status, DoctorStatus::Info);
        assert_eq!(
            validate_database_url(Some("postgres://x:y@h/db")).status,
            DoctorStatus::Ok,
        );
        assert_eq!(
            validate_database_url(Some("postgresql://x:y@h/db")).status,
            DoctorStatus::Ok,
        );
        assert_eq!(
            validate_database_url(Some("not a url")).status,
            DoctorStatus::Error,
        );
    }

    #[test]
    fn validate_database_url_rejects_non_postgres_scheme() {
        let entry = validate_database_url(Some("https://example.invalid/db"));
        assert_eq!(entry.status, DoctorStatus::Error);
        assert!(entry.detail.contains("\"https\""));
    }

    #[test]
    fn validate_object_store_partial_is_error() {
        assert_eq!(
            validate_object_store(Some("http://localhost:8080"), None).status,
            DoctorStatus::Error,
        );
        assert_eq!(
            validate_object_store(None, Some(TEST_SIGNING_SECRET)).status,
            DoctorStatus::Error,
        );
        assert_eq!(
            validate_object_store(Some("http://localhost:8080"), Some(TEST_SIGNING_SECRET)).status,
            DoctorStatus::Ok,
        );
        assert_eq!(validate_object_store(None, None).status, DoctorStatus::Info);
    }

    #[test]
    fn validate_object_store_rejects_malformed_url() {
        let entry = validate_object_store(Some("not-a-url"), Some(TEST_SIGNING_SECRET));
        assert_eq!(entry.status, DoctorStatus::Error);
        assert_eq!(entry.label, "OBJECT_STORE_BASE_URL");
    }

    #[test]
    fn validate_object_store_rejects_short_signing_secret() {
        // `LocalFsObjectStore::new` requires 32+ bytes; doctor must
        // surface that as an Error rather than rubber-stamp the URL.
        let entry = validate_object_store(Some("http://localhost:8080"), Some("too-short"));
        assert_eq!(entry.status, DoctorStatus::Error);
        assert!(entry.detail.contains("32 bytes"));
    }

    #[test]
    fn validate_object_store_rejects_base_url_with_path() {
        // Origin-only constraint from `LocalFsObjectStore::new`.
        let entry = validate_object_store(
            Some("http://localhost:8080/prefix"),
            Some(TEST_SIGNING_SECRET),
        );
        assert_eq!(entry.status, DoctorStatus::Error);
        assert!(entry.detail.contains("origin"));
    }

    #[test]
    fn validate_s3_unset_is_info() {
        let entry = validate_s3(None, None, None, None, None);
        assert_eq!(entry.status, DoctorStatus::Info);
    }

    #[test]
    fn validate_s3_partial_required_is_error() {
        let entry = validate_s3(Some("https://s3.example.invalid"), None, None, None, None);
        assert_eq!(entry.status, DoctorStatus::Error);
        assert!(entry.detail.contains("partial config"));
    }

    #[test]
    fn validate_s3_session_token_alone_is_partial_error() {
        let entry = validate_s3(None, None, None, None, Some("tok"));
        assert_eq!(entry.status, DoctorStatus::Error);
    }

    #[test]
    fn validate_s3_complete_required_is_ok_and_notes_token() {
        let entry = validate_s3(
            Some("https://s3.example.invalid"),
            Some("us-east-1"),
            Some("AKIA"),
            Some("SECRET"),
            None,
        );
        assert_eq!(entry.status, DoctorStatus::Ok);
        assert!(!entry.detail.contains("session token"));

        let entry = validate_s3(
            Some("https://s3.example.invalid"),
            Some("us-east-1"),
            Some("AKIA"),
            Some("SECRET"),
            Some("tok"),
        );
        assert_eq!(entry.status, DoctorStatus::Ok);
        assert!(entry.detail.contains("with session token"));
    }

    #[test]
    fn validate_s3_rejects_malformed_endpoint() {
        let entry = validate_s3(
            Some("not-a-url"),
            Some("us-east-1"),
            Some("AKIA"),
            Some("SECRET"),
            None,
        );
        assert_eq!(entry.status, DoctorStatus::Error);
        assert_eq!(entry.label, "S3_ENDPOINT");
    }

    #[test]
    fn validate_s3_rejects_endpoint_with_path() {
        // Origin-only constraint from `S3ObjectStore::new`.
        let entry = validate_s3(
            Some("https://s3.example.invalid/prefix"),
            Some("us-east-1"),
            Some("AKIA"),
            Some("SECRET"),
            None,
        );
        assert_eq!(entry.status, DoctorStatus::Error);
        assert!(entry.detail.contains("origin"));
    }

    #[test]
    fn doctor_report_from_snapshot_threads_through_all_validators() {
        let env = EnvSnapshot::default();
        let report = DoctorReport::from_snapshot(&env);
        assert_eq!(report.entries.len(), 5);
        assert_eq!(report.error_count(), 0);
        let labels: Vec<&str> = report.entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "MODE",
                "GATEWAY_ADDR",
                "DATABASE_URL",
                "OBJECT_STORE_*",
                "S3_*"
            ],
        );
    }
}
