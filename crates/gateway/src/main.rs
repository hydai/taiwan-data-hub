//! HTTP + MCP gateway (Axum + tower) for Taiwan Data Hub.
//!
//! Serves liveness/readiness probes plus the MCP 2025-11-25 Streamable HTTP
//! transport at `/mcp` (POST for client→server JSON-RPC, GET for the
//! backward-compat SSE upgrade). The MCP dispatcher is shared with the
//! stdio shim: both transports talk to the same `mcp_core::McpServer`, so
//! tools register once and reach every client.

use std::net::SocketAddr;

use anyhow::Context;
use axum::http::{HeaderName, Method, header};
use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::get};
use mcp_core::rmcp::model::Implementation;
use mcp_core::rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use mcp_core::{Dispatcher, McpServer};
use serde::Serialize;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;
use tracing::info;
use tracing_subscriber::EnvFilter;

const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
const GIT_SHA: &str = env!("GATEWAY_GIT_SHA");

/// Default listening address. Overridden by the `GATEWAY_ADDR` env var.
const DEFAULT_ADDR: &str = "0.0.0.0:8080";

/// Custom header per the MCP 2025-11-25 Streamable HTTP spec — used to bind
/// a session across multiple HTTP requests.
const MCP_SESSION_ID: HeaderName = HeaderName::from_static("mcp-session-id");

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

    let addr: SocketAddr = std::env::var("GATEWAY_ADDR")
        .unwrap_or_else(|_| DEFAULT_ADDR.to_owned())
        .parse()
        .context("GATEWAY_ADDR must be a valid socket address (host:port)")?;

    // Single MCP server shared by every session — Dispatcher is Arc-backed
    // so clone() in the factory is cheap. Stdio and HTTP both feed off the
    // same `tools_data::register_data_tools` helper, so tools register in
    // one place and reach every transport.
    let dispatcher = tools_data::register_data_tools(Dispatcher::builder()).build();
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
}
