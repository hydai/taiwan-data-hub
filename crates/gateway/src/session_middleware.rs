//! Session middleware for the gateway (#4.5).
//!
//! Extracts the `tdh_session` cookie from inbound requests,
//! validates it via [`auth::SessionService`], and inserts the
//! resulting [`auth::ValidatedSession`] into the request
//! extensions. Handlers that want to gate on authentication
//! extract `Extension<ValidatedSession>` (or
//! `Option<Extension<ValidatedSession>>` for "soft" gating).
//!
//! The middleware is deliberately permissive: any failure to
//! decode / authenticate the cookie results in the request
//! proceeding *anonymously*, not in a 401. That keeps the
//! `personal` mode trivially working (no cookies ever issued)
//! and lets the per-route gate (in #4.6+) decide whether to
//! reject anonymous traffic. The route gate is what produces
//! the 401; this middleware only PROVIDES the identity when
//! present.
//!
//! `#4.5` ships the middleware + helpers. The handler wiring —
//! mounting this layer on the router, building the
//! [`auth::SessionService`] at startup, issuing cookies on
//! login — lands in `#4.6` together with the API-key surface.
//! The `#[allow(dead_code)]` markers below tag the public
//! surface that the follow-up will consume.

#![allow(dead_code)]

use std::sync::Arc;

use auth::{SESSION_COOKIE_NAME, SessionService, ValidatedSession};
use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::Response;
use tracing::warn;

/// axum middleware that runs on every request. Reads the
/// `tdh_session` cookie, validates it against the [`SessionService`],
/// and on success injects the [`ValidatedSession`] into the
/// request extensions for downstream extractors.
///
/// Errors from the session repo (DB outage, etc.) are logged at
/// `warn` and the request continues anonymously — failing OPEN
/// on infra trouble matches the personal-mode behaviour and
/// prevents a DB hiccup from locking everyone out of the
/// gateway's read endpoints.
pub async fn session_middleware(
    State(svc): State<Arc<SessionService>>,
    mut req: Request,
    next: Next,
) -> Response {
    if let Some(cookie) = extract_session_cookie(req.headers()) {
        match svc.authenticate(cookie).await {
            Ok(Some(session)) => {
                req.extensions_mut().insert(session);
            }
            Ok(None) => {
                // Cookie present but invalid / expired / revoked
                // — proceed anonymously. The route gate (or a
                // dedicated /me handler) decides whether to
                // also clear the cookie via Set-Cookie; the
                // middleware itself doesn't mutate responses.
            }
            Err(e) => {
                // DB outage etc. Fail open to avoid taking down
                // public routes on a session-store hiccup. The
                // private routes that REQUIRE a session will
                // 401 because the extension wasn't inserted.
                warn!(
                    error = %e,
                    "session lookup failed; request proceeding anonymously"
                );
            }
        }
    }
    next.run(req).await
}

/// Parse the `Cookie:` header looking for the
/// `tdh_session=<value>` pair. RFC 6265 allows multiple cookies
/// separated by `; `; we walk the header value once, split on
/// `;` + trim, and return the first matching value.
///
/// Returns `None` when:
/// - There is no `Cookie` header.
/// - The header isn't valid UTF-8 (browsers shouldn't send this,
///   but `HeaderValue::to_str` can fail on arbitrary bytes).
/// - No `tdh_session=` pair appears in the value, or its value
///   is empty.
fn extract_session_cookie(headers: &HeaderMap) -> Option<&str> {
    // Walk every `Cookie:` header value the request carries. RFC
    // 6265 §5.4 says clients SHOULD send one Cookie header, but
    // some proxies (and h2/h3 implementations) legitimately
    // split the cookie set across multiple header lines —
    // checking only `headers.get` would miss the session in
    // that case.
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|hv| hv.to_str().ok())
        .find_map(|header| {
            header.split(';').find_map(|pair| {
                let pair = pair.trim();
                let (name, value) = pair.split_once('=')?;
                if name.trim() != SESSION_COOKIE_NAME {
                    return None;
                }
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            })
        })
}

/// Build the `Set-Cookie` header value for a freshly-issued
/// session. Attrs follow the spec: `HttpOnly` (no JS access),
/// `Secure` (HTTPS only — set unconditionally; the gateway is
/// always behind TLS in any non-`personal` deployment),
/// `SameSite=Lax` (lets top-level GET redirects through, blocks
/// cross-site POSTs), `Path=/`, `Max-Age=<ttl_seconds>`.
///
/// `max_age_seconds` is `u64` — RFC 6265 §5.2.2 defines
/// `Max-Age` as a non-zero positive integer; a signed type would
/// let a negative value through, which most browsers interpret
/// as "delete the cookie immediately" and would silently log
/// the user out the moment the cookie is set.
#[must_use]
pub fn build_session_cookie(cookie_value: &str, max_age_seconds: u64) -> String {
    format!(
        "{SESSION_COOKIE_NAME}={cookie_value}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age={max_age_seconds}"
    )
}

/// `Set-Cookie` for clearing the session cookie (logout). Same
/// attrs as the issue path so browsers match the cookie when
/// computing replacement; `Max-Age=0` evicts immediately.
#[must_use]
pub fn build_clear_session_cookie() -> String {
    format!("{SESSION_COOKIE_NAME}=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0")
}

/// Convenience alias the `#4.6` handler wiring extracts via
/// `axum::Extension<SessionExtension>`. Re-exporting the auth
/// crate's type under the gateway-local name keeps the dataflow
/// readable at the handler call sites.
pub type SessionExtension = ValidatedSession;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn header_map(cookie: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::COOKIE,
            HeaderValue::from_str(cookie).unwrap(),
        );
        h
    }

    #[test]
    fn extracts_single_cookie() {
        let h = header_map("tdh_session=abc123");
        assert_eq!(extract_session_cookie(&h), Some("abc123"));
    }

    #[test]
    fn extracts_session_amid_other_cookies() {
        let h = header_map("other=foo; tdh_session=abc123; tracker=bar");
        assert_eq!(extract_session_cookie(&h), Some("abc123"));
    }

    #[test]
    fn returns_none_when_session_cookie_absent() {
        let h = header_map("other=foo; tracker=bar");
        assert_eq!(extract_session_cookie(&h), None);
    }

    #[test]
    fn returns_none_for_empty_value() {
        let h = header_map("tdh_session=");
        assert_eq!(extract_session_cookie(&h), None);
    }

    #[test]
    fn returns_none_when_no_cookie_header() {
        let h = HeaderMap::new();
        assert_eq!(extract_session_cookie(&h), None);
    }

    #[test]
    fn build_session_cookie_has_required_attrs() {
        let s = build_session_cookie("tok", 1_209_600);
        assert!(s.contains("HttpOnly"));
        assert!(s.contains("Secure"));
        assert!(s.contains("SameSite=Lax"));
        assert!(s.contains("Path=/"));
        assert!(s.contains("Max-Age=1209600"));
        assert!(s.starts_with("tdh_session=tok;"));
    }

    #[test]
    fn build_clear_session_cookie_uses_zero_max_age() {
        let s = build_clear_session_cookie();
        assert!(s.contains("Max-Age=0"));
        assert!(s.starts_with("tdh_session=;"));
    }
}
