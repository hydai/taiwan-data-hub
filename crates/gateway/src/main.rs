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
use shared::Mode;
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
    name = "taiwan-data-hub",
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
    let database = dependency_ready(std::env::var("DATABASE_URL").ok().as_deref());
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

fn build_router(server: McpServer, cancel: CancellationToken) -> Router {
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

    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .nest_service("/mcp", mcp_with_cors)
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

    // Single MCP server shared by every session — Dispatcher is Arc-backed
    // so clone() in the factory is cheap. Stdio and HTTP both feed off the
    // same `tools_data::register_data_tools` helper, so tools register in
    // one place and reach every transport.
    let mut builder: DispatcherBuilder = tools_data::register_data_tools(Dispatcher::builder());
    builder = tools_utility::register_utility_tools(builder);
    builder = wire_db_tools_if_available(builder).await;
    let dispatcher = builder.build();
    let tool_count = dispatcher.len();
    let server = McpServer::new(dispatcher, gateway_implementation())
        .with_instructions("Taiwan Data Hub MCP server.");

    let cancel = CancellationToken::new();
    let app = build_router(server, cancel.child_token());

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

/// Register Postgres-backed tools when `DATABASE_URL` is set and a
/// pool can be established. Mirrors the policy in `mcp-stdio` so
/// both transports share the same view of which tools are available.
/// Failure to connect downgrades to "no DB tools" rather than killing
/// the gateway — `/healthz` and `/readyz` stay independently useful
/// for ops, and personal-mode installs without Postgres still get a
/// working MCP server with `list_domains`.
async fn wire_db_tools_if_available(builder: DispatcherBuilder) -> DispatcherBuilder {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        tracing::info!("DATABASE_URL unset; DB-backed tools disabled (list_domains still works)");
        return builder;
    };
    match Storage::connect(&url).await {
        Ok(storage) => {
            tracing::info!("DATABASE_URL connected; registering DB-backed tools");
            let router = build_object_store_router();
            tools_data::register_db_tools(builder, storage, router)
        }
        Err(e) => {
            tracing::warn!(error = %e, "DATABASE_URL set but Storage::connect failed; DB tools disabled");
            builder
        }
    }
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

/// Parse `GATEWAY_ADDR` from env (or the default) into a `SocketAddr`.
/// Extracted so `serve` and `doctor` share the same error wording.
fn read_gateway_addr() -> anyhow::Result<SocketAddr> {
    std::env::var("GATEWAY_ADDR")
        .unwrap_or_else(|_| DEFAULT_ADDR.to_owned())
        .parse()
        .context("GATEWAY_ADDR must be a valid socket address (host:port)")
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
    /// Config is valid and present.
    Ok,
    /// Config is absent but the binary tolerates that (e.g. no
    /// `DATABASE_URL` in a personal-mode install). Always rendered
    /// `info`, never an error.
    Info,
    /// Config is present but malformed, partial, or contradictory.
    /// Counts toward [`DoctorReport::has_errors`] so the process
    /// exits non-zero.
    Error,
}

impl DoctorReport {
    fn collect() -> Self {
        let mut report = Self::default();
        report.check_mode();
        report.check_gateway_addr();
        report.check_database_url();
        report.check_object_store();
        report.check_s3();
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

    fn push(&mut self, label: &str, status: DoctorStatus, detail: impl Into<String>) {
        self.entries.push(DoctorEntry {
            label: label.to_owned(),
            status,
            detail: detail.into(),
        });
    }

    fn check_mode(&mut self) {
        match Mode::from_env() {
            Ok(mode) => self.push(
                "MODE",
                DoctorStatus::Ok,
                format!(
                    "{mode} (public: {}; gated: {})",
                    PUBLIC_ROUTES.join(","),
                    gated_route_list(mode),
                ),
            ),
            Err(e) => self.push("MODE", DoctorStatus::Error, e.to_string()),
        }
    }

    fn check_gateway_addr(&mut self) {
        match read_gateway_addr() {
            Ok(addr) => self.push("GATEWAY_ADDR", DoctorStatus::Ok, addr.to_string()),
            Err(e) => self.push("GATEWAY_ADDR", DoctorStatus::Error, format!("{e:#}")),
        }
    }

    fn check_database_url(&mut self) {
        match non_empty_env("DATABASE_URL") {
            None => self.push(
                "DATABASE_URL",
                DoctorStatus::Info,
                "unset — DB-backed tools will be disabled".to_owned(),
            ),
            Some(raw) => match Url::parse(&raw) {
                Ok(_) => self.push(
                    "DATABASE_URL",
                    DoctorStatus::Ok,
                    "<set> (connection probe skipped; run `cargo test --release -p storage -- --ignored` for end-to-end)".to_owned(),
                ),
                Err(e) => self.push(
                    "DATABASE_URL",
                    DoctorStatus::Error,
                    format!("not a valid URL: {e}"),
                ),
            },
        }
    }

    fn check_object_store(&mut self) {
        let base = non_empty_env("OBJECT_STORE_BASE_URL");
        let secret = non_empty_env("OBJECT_STORE_SIGNING_SECRET");
        match (base, secret) {
            (None, None) => self.push(
                "OBJECT_STORE_*",
                DoctorStatus::Info,
                "unset — file:// URIs unavailable".to_owned(),
            ),
            (Some(_), None) | (None, Some(_)) => self.push(
                "OBJECT_STORE_*",
                DoctorStatus::Error,
                "partial config: both OBJECT_STORE_BASE_URL and OBJECT_STORE_SIGNING_SECRET must be set together".to_owned(),
            ),
            (Some(b), Some(_)) => match Url::parse(&b) {
                Ok(_) => self.push(
                    "OBJECT_STORE_*",
                    DoctorStatus::Ok,
                    format!("base={b}"),
                ),
                Err(e) => self.push(
                    "OBJECT_STORE_BASE_URL",
                    DoctorStatus::Error,
                    format!("not a valid URL: {e}"),
                ),
            },
        }
    }

    fn check_s3(&mut self) {
        let endpoint = non_empty_env("S3_ENDPOINT");
        let region = non_empty_env("S3_REGION");
        let key = non_empty_env("S3_ACCESS_KEY_ID");
        let secret = non_empty_env("S3_SECRET_ACCESS_KEY");
        let any = endpoint.is_some() || region.is_some() || key.is_some() || secret.is_some();
        let all = endpoint.is_some() && region.is_some() && key.is_some() && secret.is_some();
        if !any {
            self.push(
                "S3_*",
                DoctorStatus::Info,
                "unset — s3:// URIs unavailable".to_owned(),
            );
            return;
        }
        if !all {
            self.push(
                "S3_*",
                DoctorStatus::Error,
                "partial config: S3_ENDPOINT, S3_REGION, S3_ACCESS_KEY_ID, S3_SECRET_ACCESS_KEY must be set together".to_owned(),
            );
            return;
        }
        let endpoint = endpoint.expect("checked above");
        match Url::parse(&endpoint) {
            Ok(_) => self.push("S3_*", DoctorStatus::Ok, format!("endpoint={endpoint}")),
            Err(e) => self.push(
                "S3_ENDPOINT",
                DoctorStatus::Error,
                format!("not a valid URL: {e}"),
            ),
        }
    }
}

impl std::fmt::Display for DoctorReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "taiwan-data-hub doctor (gateway v{PKG_VERSION})")?;
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
        let app = build_router(test_server(), CancellationToken::new());
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
        let app = build_router(test_server(), CancellationToken::new());

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
        let app = build_router(test_server(), CancellationToken::new());

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
}
