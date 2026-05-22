//! Rate-limit middleware (#4.7).
//!
//! Two `axum::middleware::from_fn_with_state` middleware
//! functions backed by an [`auth::RateLimiter`]:
//!
//!   * [`ip_rate_limit_middleware`] applies to every request the
//!     gateway accepts and caps per-IP traffic at
//!     [`auth::DEFAULT_IP_RPM`] (60/min). Client IP is read from
//!     the request via [`extract_client_ip`], which walks the
//!     standard reverse-proxy header chain before falling back
//!     to the connection's peer address.
//!   * [`api_key_rate_limit_middleware`] is mounted DOWNSTREAM
//!     of the session middleware so it sees the
//!     [`auth::ValidatedSession`] extension. It picks the per-
//!     tier RPM via [`auth::tier_rpm`] using the session's
//!     active key tier (placeholder `"free"` until the
//!     `ValidatedSession` carries the tier inline — that change
//!     lives outside this PR's scope).
//!
//! Both middleware variants return the same canonical 429
//! response shape ([`build_rate_limit_response`] is the single
//! source of truth) and emit the `X-RateLimit-*` headers
//! alongside the optional `Retry-After` so clients that don't
//! special-case the gateway can still pace themselves via the
//! IETF draft headers.
//!
//! The third layer (`query_rows` tool-specific stricter limit)
//! lives in `tools-data` as an explicit guard inside the tool
//! body — see the note on Layer 3 in [`auth::rate_limit`] for
//! why it isn't a tower middleware.

use std::net::IpAddr;
use std::sync::Arc;

use auth::{DEFAULT_IP_RPM, RateLimitOutcome, RateLimiter, ValidatedSession, tier_rpm};
use axum::extract::{ConnectInfo, Extension, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use std::net::SocketAddr;
use tracing::warn;

/// Canonical `X-RateLimit-Limit` header name (kebab-case per
/// the IETF draft).
const HEADER_LIMIT: &str = "x-ratelimit-limit";
/// Canonical `X-RateLimit-Remaining` header name.
const HEADER_REMAINING: &str = "x-ratelimit-remaining";
/// Canonical `X-RateLimit-Reset` header name. We emit the
/// delta-seconds form (RFC-7231-style delta) to match the
/// `Retry-After` semantics — clients that read either end up
/// with the same number.
const HEADER_RESET: &str = "x-ratelimit-reset";

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
    let ip = extract_client_ip(req.headers(), peer_addr);
    let key = format!("ip:{ip}");
    let outcome = match limiter.check(&key, DEFAULT_IP_RPM, Utc::now()).await {
        Ok(o) => o,
        Err(e) => {
            // Fail OPEN on rate-limiter outages: a DB hiccup
            // should not lock everyone out of the gateway. The
            // log captures the cause for ops; the request
            // proceeds without rate-limit headers because we
            // don't have a trustworthy outcome to report.
            warn!(error = %e, "ip rate-limit lookup failed; failing open");
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

/// Middleware that throttles per-API-key traffic by tier.
/// Mounted DOWNSTREAM of session / API-key auth so the request
/// carries an [`auth::ValidatedSession`] extension. Anonymous
/// requests (no session) skip this layer entirely — they're
/// already covered by the IP middleware above.
///
/// Tier is currently sourced from a placeholder default
/// (`"free"`); a follow-up will plumb the active key's
/// [`storage::ApiKeyRow::rate_limit_tier`] through the
/// [`ValidatedSession`] extension. The middleware shape doesn't
/// change when that lands — only the `tier_rpm(...)` argument
/// does.
pub async fn api_key_rate_limit_middleware(
    State(limiter): State<Arc<dyn RateLimiter>>,
    session: Option<Extension<ValidatedSession>>,
    req: Request,
    next: Next,
) -> Response {
    let Some(Extension(session)) = session else {
        // Anonymous request — IP middleware already throttled
        // it. Pass through without bumping per-key counters.
        return next.run(req).await;
    };
    let key = format!("key:{}", session.user_id);
    // Placeholder tier until ValidatedSession carries it; see
    // doc comment above.
    let rpm = tier_rpm("free");
    let outcome = match limiter.check(&key, rpm, Utc::now()).await {
        Ok(o) => o,
        Err(e) => {
            warn!(error = %e, "api-key rate-limit lookup failed; failing open");
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
/// Header precedence:
///   1. `X-Forwarded-For` — first comma-separated entry, which
///      is the originating client per RFC 7239 §5.2.
///   2. `X-Real-IP` — single value, common with nginx /
///      Cloudflare.
///   3. Connection peer — the immediate TCP source, used when
///      the gateway is exposed directly (no proxy).
///
/// Returns an [`IpAddr`] so the caller can format it
/// canonically (no risk of two different string forms for the
/// same IP — `::ffff:127.0.0.1` vs `127.0.0.1` collapse to the
/// same `IpAddr`).
pub fn extract_client_ip(headers: &HeaderMap, peer: SocketAddr) -> IpAddr {
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
    peer.ip()
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
    fn extract_client_ip_prefers_forwarded_for_first_entry() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.42, 10.0.0.1, 10.0.0.2"),
        );
        let ip = extract_client_ip(&headers, peer("127.0.0.1:1234"));
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(203, 0, 113, 42)));
    }

    #[test]
    fn extract_client_ip_falls_back_to_x_real_ip() {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.7"));
        let ip = extract_client_ip(&headers, peer("127.0.0.1:1234"));
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7)));
    }

    #[test]
    fn extract_client_ip_falls_back_to_peer_when_headers_missing() {
        let headers = HeaderMap::new();
        let ip = extract_client_ip(&headers, peer("[2001:db8::1]:8080"));
        assert_eq!(ip, IpAddr::V6("2001:db8::1".parse::<Ipv6Addr>().unwrap()));
    }

    #[test]
    fn extract_client_ip_skips_unparseable_forwarded_entry() {
        let mut headers = HeaderMap::new();
        // Looks like a header but doesn't parse — should
        // gracefully fall through to peer rather than panic.
        headers.insert("x-forwarded-for", HeaderValue::from_static("not-an-ip"));
        let ip = extract_client_ip(&headers, peer("127.0.0.1:1234"));
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
