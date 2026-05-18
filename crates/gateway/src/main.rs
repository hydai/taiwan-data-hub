//! HTTP + MCP gateway (Axum + tower) for Taiwan Data Hub.
//!
//! Currently ships only the liveness/readiness endpoints from M0 #0.4.
//! The MCP and REST surfaces are added in later milestones — see
//! `docs/DESIGN.md`.

use std::net::SocketAddr;

use anyhow::Context;
use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::get};
use serde::Serialize;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
const GIT_SHA: &str = env!("GATEWAY_GIT_SHA");

/// Default listening address. Overridden by the `GATEWAY_ADDR` env var.
const DEFAULT_ADDR: &str = "0.0.0.0:8080";

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

fn build_router() -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
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

    let app = build_router();

    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;

    info!(
        version = PKG_VERSION,
        build_sha = GIT_SHA,
        addr = %addr,
        "gateway listening"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum server crashed")?;

    Ok(())
}

/// Returns when the process receives SIGINT or SIGTERM so axum can
/// drain in-flight requests instead of dropping connections. Critical
/// for `docker stop` (SIGTERM) and Ctrl-C (SIGINT) in dev.
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");

    tokio::select! {
        _ = sigterm.recv() => info!("received SIGTERM, shutting down"),
        _ = sigint.recv() => info!("received SIGINT, shutting down"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::ServiceExt;

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn healthz_returns_200_with_version_and_sha() {
        let app = build_router();
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
}
