//! Rate-limit middleware (#4.7).
//!
//! Two `axum::middleware::from_fn_with_state` middleware
//! functions backed by an [`auth::RateLimiter`]:
//!
//!   * [`ip_rate_limit_middleware`] applies to every request the
//!     gateway accepts and caps per-IP traffic at
//!     [`auth::DEFAULT_IP_RPM`] (60/min). Client IP is read from
//!     the request via [`extract_client_ip`], which honours
//!     reverse-proxy headers ONLY when `TRUST_PROXY_HEADERS=1`
//!     is set (the default is to use the connection peer
//!     because a client behind no proxy can spoof
//!     `X-Forwarded-For` to evade per-IP throttling).
//!   * [`session_rate_limit_middleware`] is mounted DOWNSTREAM
//!     of the session middleware so it sees the
//!     [`auth::ValidatedSession`] extension. It keys the limiter
//!     by SESSION USER id (`session:<user_uuid>`) and applies a
//!     placeholder tier-mapped RPM (`free` for now). When
//!     per-key auth wires the active `mcp_api_keys`
//!     `rate_limit_tier` into the request context (separate PR),
//!     this middleware switches to keying by api-key id and
//!     using the real tier without changing its shape.
//!
//! Both middleware variants return the same canonical 429
//! response shape ([`build_rate_limit_response`] is the single
//! source of truth) and emit the legacy `X-RateLimit-*`
//! headers alongside the `Retry-After` so clients can pace
//! themselves before they hit the cap. (The IETF draft uses
//! un-prefixed `RateLimit-*`; we emit the `X-`-prefixed
//! variants because every major client library still reads
//! those — switching to the draft names is a future
//! compatibility-break decision.)
//!
//! The third layer (`query_rows` tool-specific stricter limit)
//! is deferred — see the note on Layer 3 in [`auth::rate_limit`]
//! for why it needs per-caller plumbing inside the MCP
//! dispatcher that doesn't ship in this PR.

use std::net::IpAddr;
use std::sync::Arc;

use auth::{DEFAULT_IP_RPM, RateLimitOutcome, RateLimiter, ValidatedSession, tier_rpm};
use axum::extract::{ConnectInfo, Extension, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use std::net::SocketAddr;
use tracing::debug;

/// Legacy `X-RateLimit-Limit` header name. The IETF draft
/// uses an un-prefixed `RateLimit-Limit`; this code emits
/// the `X-`-prefixed variant because every major client
/// library still reads it.
const HEADER_LIMIT: &str = "x-ratelimit-limit";
/// Legacy `X-RateLimit-Remaining` header name (same rationale
/// as [`HEADER_LIMIT`]).
const HEADER_REMAINING: &str = "x-ratelimit-remaining";
/// Legacy `X-RateLimit-Reset` header name. We emit the
/// delta-seconds form (RFC-7231-style delta) to match the
/// `Retry-After` semantics — clients that read either end up
/// with the same number.
const HEADER_RESET: &str = "x-ratelimit-reset";

/// Env var that opts into honouring `X-Forwarded-For` /
/// `X-Real-IP` when extracting the client IP. Default OFF so
/// a gateway exposed directly (no trusted reverse proxy
/// stripping inbound forwarded-for headers) can't be spoofed
/// into thinking each request comes from a different IP.
/// Operators behind a real proxy set this to `1` (or any
/// non-empty value) to re-enable the header chain.
const ENV_TRUST_PROXY_HEADERS: &str = "TRUST_PROXY_HEADERS";

/// Middleware that throttles per-IP traffic at the gateway's
/// outermost boundary. Mounted on the top-level router so it
/// applies to `/healthz`, `/readyz`, `/mcp`, and every
/// subrouter beneath. The throttle is intentionally coarse
/// (60/min) because (a) it's the only defence in personal-mode
/// where no auth is wired up, and (b) post-auth layers tighten
/// further on the same request.
///
/// Production always populates `ConnectInfo<SocketAddr>` via
/// the `into_make_service_with_connect_info` plumbing in
/// `main`; the middleware reads the extension directly so
/// tower-tests (which don't go through that plumbing) can
/// still drive this code without producing a 500 on a missing
/// extension — they fall back to a `0.0.0.0` placeholder which
/// keeps all test requests on a single counter key (fine for
/// the test harness since none should hit the cap).
pub async fn ip_rate_limit_middleware(
    State(limiter): State<Arc<dyn RateLimiter>>,
    req: Request,
    next: Next,
) -> Response {
    let peer_addr = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or_else(
            || SocketAddr::from(([0, 0, 0, 0], 0)),
            |ConnectInfo(addr)| *addr,
        );
    let ip = extract_client_ip(req.headers(), peer_addr, trust_proxy_headers());
    let key = format!("ip:{ip}");
    let outcome = match limiter.check(&key, DEFAULT_IP_RPM, Utc::now()).await {
        Ok(o) => o,
        Err(e) => {
            // Fail OPEN on rate-limiter outages: a DB hiccup
            // should not lock everyone out of the gateway. Log
            // at `debug!` (not `warn!`) because every request
            // during a sustained outage takes this path — a
            // per-request `warn!` would flood the log faster
            // than ops can read it. The outage signal belongs
            // in metrics (a session-lookup-failure counter,
            // landing alongside #4.5/#4.6 observability) and
            // in the storage health probe, not log volume.
            debug!(error = %e, "ip rate-limit lookup failed; failing open");
            return next.run(req).await;
        }
    };
    if !outcome.allowed {
        return build_rate_limit_response(outcome);
    }
    let mut response = next.run(req).await;
    attach_rate_limit_headers(response.headers_mut(), outcome);
    response
}

/// Middleware that throttles per-session traffic by tier.
/// Mounted DOWNSTREAM of session auth so the request carries
/// an [`auth::ValidatedSession`] extension. Anonymous
/// requests (no session) skip this layer entirely — they're
/// already covered by the IP middleware above.
///
/// Keyed by SESSION USER id (`session:<user_uuid>`) — NOT the
/// active API-key id, because [`ValidatedSession`] doesn't
/// carry the api-key context (the session is the cookie
/// auth's primary surface; per-API-key auth lands in a
/// follow-up PR). Tier is sourced from a placeholder default
/// (`"free"`); a follow-up will plumb the active key's
/// [`storage::ApiKeyRow::rate_limit_tier`] through the
/// request context and switch the key prefix to `key:` and
/// the tier to the real value. The middleware shape doesn't
/// change when that lands.
pub async fn session_rate_limit_middleware(
    State(limiter): State<Arc<dyn RateLimiter>>,
    session: Option<Extension<ValidatedSession>>,
    req: Request,
    next: Next,
) -> Response {
    let Some(Extension(session)) = session else {
        // Anonymous request — IP middleware already throttled
        // it. Pass through without bumping per-session counters.
        return next.run(req).await;
    };
    let key = format!("session:{}", session.user_id);
    // Placeholder tier until per-API-key auth lands; see doc
    // comment above.
    let rpm = tier_rpm("free");
    let outcome = match limiter.check(&key, rpm, Utc::now()).await {
        Ok(o) => o,
        Err(e) => {
            // `debug!` for the same reason as the IP middleware
            // — sustained outage flooding via per-request
            // `warn!` is the wrong substrate. Metrics +
            // health-probe alerts are the right outage signal.
            debug!(error = %e, "session rate-limit lookup failed; failing open");
            return next.run(req).await;
        }
    };
    if !outcome.allowed {
        return build_rate_limit_response(outcome);
    }
    let mut response = next.run(req).await;
    attach_rate_limit_headers(response.headers_mut(), outcome);
    response
}

/// Extract the client IP from `headers` (proxy chain) with a
/// fallback to the connection's peer address.
///
/// `trust_proxy_headers` gates whether the de-facto reverse-
/// proxy headers (`X-Forwarded-For` first entry, then
/// `X-Real-IP`) are honoured. When `false` (the default,
/// safe-by-default), the function ignores those headers
/// entirely and uses `peer.ip()` — because a gateway exposed
/// directly without a trusted proxy stripping inbound
/// forwarded-for headers would otherwise let a client spoof
/// its per-IP counter key by setting the header itself.
/// Operators behind nginx / Cloudflare / Caddy set
/// `TRUST_PROXY_HEADERS=1` to opt in.
///
/// Neither header is in any IETF RFC: `X-Forwarded-For` is the
/// de-facto Squid convention and `X-Real-IP` is the nginx
/// idiom. The standardised `Forwarded` header (RFC 7239) is
/// not currently parsed — when production traffic shows it's
/// being set we'll add it to the chain.
///
/// Returns an [`IpAddr`] so the caller can format it
/// canonically (no risk of two different string forms for the
/// same IP — `::ffff:127.0.0.1` vs `127.0.0.1` collapse to the
/// same `IpAddr`).
pub fn extract_client_ip(
    headers: &HeaderMap,
    peer: SocketAddr,
    trust_proxy_headers: bool,
) -> IpAddr {
    if trust_proxy_headers {
        if let Some(forwarded) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(first) = forwarded.split(',').next() {
                if let Ok(parsed) = first.trim().parse::<IpAddr>() {
                    return parsed;
                }
            }
        }
        if let Some(real) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()) {
            if let Ok(parsed) = real.trim().parse::<IpAddr>() {
                return parsed;
            }
        }
    }
    peer.ip()
}

/// Read the `TRUST_PROXY_HEADERS` env var. Any non-empty value
/// counts as "trust" — operators behind a real proxy set this
/// to `1` once at deploy time and forget about it. Reading
/// the env var per request is cheap (the OS caches it) and
/// avoids plumbing config through every middleware layer.
fn trust_proxy_headers() -> bool {
    std::env::var(ENV_TRUST_PROXY_HEADERS)
        .ok()
        .is_some_and(|v| !v.trim().is_empty())
}

/// Build the canonical 429 response. Single source of truth so
/// the two middleware variants (and any future caller) can't
/// disagree on body shape, headers, or status code.
#[must_use]
pub fn build_rate_limit_response(outcome: RateLimitOutcome) -> Response {
    let body = serde_json::json!({
        "error": "rate_limited",
        "message": "Rate limit exceeded. Slow down and try again.",
        "limit": outcome.limit,
        "retry_after_seconds": outcome.retry_after_seconds,
    });
    let mut response = (StatusCode::TOO_MANY_REQUESTS, axum::Json(body)).into_response();
    attach_rate_limit_headers(response.headers_mut(), outcome);
    if let Ok(value) = HeaderValue::from_str(&outcome.retry_after_seconds.to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

/// Attach the IETF-draft `X-RateLimit-*` headers to `headers`.
/// Used on both the allowed and rejected paths so clients can
/// pace themselves before they hit the cap.
fn attach_rate_limit_headers(headers: &mut HeaderMap, outcome: RateLimitOutcome) {
    insert_numeric(headers, HEADER_LIMIT, outcome.limit.into());
    insert_numeric(headers, HEADER_REMAINING, outcome.remaining.into());
    insert_numeric(headers, HEADER_RESET, outcome.retry_after_seconds);
}

/// Helper: insert a numeric header value, swallowing the
/// (impossible-for-numbers) `InvalidHeaderValue` so the
/// happy path doesn't need a `?`. Numbers are always valid
/// header values, but the API still returns `Result`.
fn insert_numeric(headers: &mut HeaderMap, name: &'static str, value: u64) {
    if let Ok(v) = HeaderValue::from_str(&value.to_string()) {
        headers.insert(header::HeaderName::from_static(name), v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn peer(addr: &str) -> SocketAddr {
        addr.parse().unwrap()
    }

    #[test]
    fn extract_client_ip_prefers_forwarded_for_first_entry_when_trusted() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.42, 10.0.0.1, 10.0.0.2"),
        );
        let ip = extract_client_ip(&headers, peer("127.0.0.1:1234"), true);
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(203, 0, 113, 42)));
    }

    #[test]
    fn extract_client_ip_falls_back_to_x_real_ip_when_trusted() {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.7"));
        let ip = extract_client_ip(&headers, peer("127.0.0.1:1234"), true);
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7)));
    }

    #[test]
    fn extract_client_ip_falls_back_to_peer_when_headers_missing() {
        let headers = HeaderMap::new();
        let ip = extract_client_ip(&headers, peer("[2001:db8::1]:8080"), true);
        assert_eq!(ip, IpAddr::V6("2001:db8::1".parse::<Ipv6Addr>().unwrap()));
    }

    #[test]
    fn extract_client_ip_skips_unparseable_forwarded_entry() {
        let mut headers = HeaderMap::new();
        // Looks like a header but doesn't parse — should
        // gracefully fall through to peer rather than panic.
        headers.insert("x-forwarded-for", HeaderValue::from_static("not-an-ip"));
        let ip = extract_client_ip(&headers, peer("127.0.0.1:1234"), true);
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::LOCALHOST));
    }

    #[test]
    fn extract_client_ip_ignores_proxy_headers_when_not_trusted() {
        // The R1 fix: a client behind no trusted proxy can set
        // `X-Forwarded-For` themselves and otherwise spoof the
        // per-IP counter key. With `trust_proxy_headers=false`
        // the helper ignores the header entirely and uses the
        // connection peer instead.
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.42"));
        headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.7"));
        let ip = extract_client_ip(&headers, peer("127.0.0.1:1234"), false);
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::LOCALHOST));
    }

    #[test]
    fn build_rate_limit_response_sets_429_and_headers() {
        let outcome = RateLimitOutcome {
            allowed: false,
            limit: 60,
            remaining: 0,
            retry_after_seconds: 42,
        };
        let r = build_rate_limit_response(outcome);
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            r.headers().get("retry-after").map(|v| v.to_str().unwrap()),
            Some("42")
        );
        assert_eq!(
            r.headers()
                .get("x-ratelimit-limit")
                .map(|v| v.to_str().unwrap()),
            Some("60")
        );
        assert_eq!(
            r.headers()
                .get("x-ratelimit-remaining")
                .map(|v| v.to_str().unwrap()),
            Some("0")
        );
        assert_eq!(
            r.headers()
                .get("x-ratelimit-reset")
                .map(|v| v.to_str().unwrap()),
            Some("42")
        );
    }

    #[test]
    fn attach_rate_limit_headers_emits_all_three_on_allowed_path() {
        let outcome = RateLimitOutcome {
            allowed: true,
            limit: 60,
            remaining: 17,
            retry_after_seconds: 30,
        };
        let mut headers = HeaderMap::new();
        attach_rate_limit_headers(&mut headers, outcome);
        assert_eq!(
            headers
                .get("x-ratelimit-limit")
                .map(|v| v.to_str().unwrap()),
            Some("60")
        );
        assert_eq!(
            headers
                .get("x-ratelimit-remaining")
                .map(|v| v.to_str().unwrap()),
            Some("17")
        );
        assert_eq!(
            headers
                .get("x-ratelimit-reset")
                .map(|v| v.to_str().unwrap()),
            Some("30")
        );
    }
}
