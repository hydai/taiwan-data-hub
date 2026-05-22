//! Public `/api/v1/*` HTTP routes (#4.8).
//!
//! Two endpoints today, both consumed by the `SvelteKit` layout
//! load function to drive auth-conditional rendering:
//!
//!   * `GET /api/v1/config` — returns the gateway's operating
//!     mode (`personal` vs `multi-user`). The `SvelteKit`
//!     `+layout.server.ts` reads this instead of duplicating
//!     the `MODE` env var on the web side; that way a single
//!     deploy can serve both modes by flipping the gateway's
//!     env without rebuilding the frontend. Always mounted
//!     at the gateway's top level — no auth or DB required.
//!   * `GET /api/v1/me` — returns the active user's identity
//!     when the request carries a valid session cookie,
//!     `{ user: null }` otherwise. Always 200 (not 401) so the
//!     layout load doesn't have to special-case
//!     non-authenticated traffic — the body shape is the
//!     discriminant. Mounted ONLY when the auth subrouter is
//!     active (`Storage` + `SESSION_HMAC_KEY` both available);
//!     in personal mode the frontend layout knows from
//!     `/api/v1/config` not to fetch `/me` and the path 404s
//!     instead.

use auth::ValidatedSession;
use axum::Json;
use axum::extract::{Extension, State};
use chrono::{DateTime, Utc};
use serde::Serialize;
use shared::Mode;
use uuid::Uuid;

/// JSON body for `GET /api/v1/config`. Today only the
/// operating mode is exposed; future config flags
/// (feature toggles, branding, etc.) can extend the struct
/// without breaking existing clients because added fields are
/// backwards-compatible additions.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ConfigResponse {
    /// `"personal"` or `"multi-user"`. The shape matches
    /// `Mode::as_str` exactly so a new variant on the Rust
    /// side surfaces as a new string value here without
    /// touching this module.
    pub mode: &'static str,
}

/// JSON body for `GET /api/v1/me`. The `user` field is the
/// discriminant — `None` means anonymous (no session or
/// expired session), `Some(_)` means authenticated. This
/// shape avoids the 401-vs-200 split the layout would
/// otherwise have to special-case.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct MeResponse {
    pub user: Option<MeUser>,
}

/// Minimal user identity payload. Only fields the layout
/// actually consumes — no email or profile data here.
/// Richer user-profile endpoints land separately when needed.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct MeUser {
    pub user_id: Uuid,
    /// Session created-at (NOT user created-at). Used by the
    /// layout to render "signed in since …" in a future
    /// follow-up; included now so the response shape doesn't
    /// have to change later.
    pub session_created_at: DateTime<Utc>,
    /// Server-side sliding-window expiry of the active
    /// session. Lets the layout schedule a refresh
    /// proactively before the cookie goes stale (separate
    /// follow-up; field is forward-compat).
    pub session_expires_at: DateTime<Utc>,
}

/// Build the `/api/v1/config` subrouter. Always mounted at
/// the gateway's top level; no auth or DB dependency, so it
/// serves in personal mode too.
pub fn config_router(mode: Mode) -> axum::Router {
    axum::Router::new()
        .route("/api/v1/config", axum::routing::get(get_config))
        .with_state(mode)
}

/// `MethodRouter` for the session-gated `/api/v1/me` handler.
/// Returned uncomposed (rather than as a full `Router`) so
/// the caller can wrap it with the session middleware before
/// mounting — the handler itself extracts via
/// `Option<Extension<ValidatedSession>>` so it needs no
/// router-level state, just the extension the session
/// middleware injects upstream. Exposing it as a bare
/// `MethodRouter<()>` lets `main.rs` mount it alongside the
/// api-keys subrouter under the same session middleware
/// layer.
pub fn me_handler() -> axum::routing::MethodRouter<()> {
    axum::routing::get(get_me)
}

async fn get_config(State(mode): State<Mode>) -> Json<ConfigResponse> {
    Json(ConfigResponse {
        mode: mode.as_str(),
    })
}

/// `/api/v1/me` — returns `{ user: <some|null> }`. The
/// `Option<Extension>` extractor avoids the 401-vs-200 split
/// the layout would otherwise have to special-case;
/// anonymous traffic gets the same body shape with
/// `user: null`.
async fn get_me(session: Option<Extension<ValidatedSession>>) -> Json<MeResponse> {
    let user = session.map(|Extension(s)| MeUser {
        user_id: s.user_id,
        session_created_at: s.created_at,
        session_expires_at: s.expires_at,
    });
    Json(MeResponse { user })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_response_personal_mode() {
        let body = ConfigResponse {
            mode: Mode::Personal.as_str(),
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json, serde_json::json!({ "mode": "personal" }));
    }

    #[test]
    fn config_response_multi_user_mode() {
        let body = ConfigResponse {
            mode: Mode::MultiUser.as_str(),
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json, serde_json::json!({ "mode": "multi-user" }));
    }

    #[test]
    fn me_response_anonymous_serialises_as_null_user() {
        let body = MeResponse { user: None };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json, serde_json::json!({ "user": null }));
    }

    #[test]
    fn me_response_authenticated_shape() {
        let user_id = Uuid::nil();
        let now = "2026-05-23T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let body = MeResponse {
            user: Some(MeUser {
                user_id,
                session_created_at: now,
                session_expires_at: now,
            }),
        };
        let json = serde_json::to_value(&body).unwrap();
        let obj = json.get("user").and_then(|v| v.as_object()).unwrap();
        assert!(obj.contains_key("user_id"));
        assert!(obj.contains_key("session_created_at"));
        assert!(obj.contains_key("session_expires_at"));
    }
}
