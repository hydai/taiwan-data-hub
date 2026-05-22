//! `/v1/api-keys` HTTP routes (#4.6).
//!
//! Per-user API-key CRUD. All routes are session-gated: the
//! upstream [`crate::session_middleware`] either injects a
//! [`auth::ValidatedSession`] extension on a valid cookie or
//! leaves it absent. These handlers extract via `Option<…>` so
//! they can return `401` (instead of letting axum 500 on a
//! missing extractor) when the cookie was missing / invalid /
//! revoked / expired.
//!
//! Body / response shapes are intentionally minimal and JSON —
//! the `SvelteKit` Account page (#4.6 frontend) is the primary
//! consumer; future MCP / CLI clients can read the same JSON.

use std::sync::Arc;

use auth::{ApiKeyService, AuthError, DEFAULT_RATE_LIMIT_TIER, ValidatedSession};
use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use storage::ApiKeyRow;
use tracing::{debug, warn};
use uuid::Uuid;

/// JSON body for `POST /v1/api-keys`. `rate_limit_tier`
/// defaults to [`DEFAULT_RATE_LIMIT_TIER`] when omitted so a
/// freshly-onboarded user can issue a key with no body fields
/// other than `name`. `scopes` defaults to empty for the same
/// reason.
#[derive(Debug, Deserialize)]
pub struct CreateApiKeyRequest {
    pub name: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub rate_limit_tier: Option<String>,
}

/// JSON response for `POST /v1/api-keys` and
/// `POST /v1/api-keys/{id}/rotate`. The `cleartext` field is
/// SHOWN ONCE — the client (typically the `SvelteKit` Account
/// page) renders it in a "copy me, you will not see it again"
/// modal and then drops it. Subsequent `GET /v1/api-keys` calls
/// do NOT echo cleartext for any key.
#[derive(Debug, Serialize)]
pub struct CreateApiKeyResponse {
    pub id: Uuid,
    pub cleartext: String,
    pub key_prefix: String,
}

/// Element of the `GET /v1/api-keys` response array. Never
/// includes the cleartext.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ApiKeySummary {
    pub id: Uuid,
    pub name: String,
    pub key_prefix: String,
    pub scopes: Vec<String>,
    pub rate_limit_tier: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl From<ApiKeyRow> for ApiKeySummary {
    fn from(row: ApiKeyRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            key_prefix: row.key_prefix,
            scopes: row.scopes,
            rate_limit_tier: row.rate_limit_tier,
            created_at: row.created_at,
            last_used_at: row.last_used_at,
            revoked_at: row.revoked_at,
        }
    }
}

/// Build the `/v1/api-keys` subrouter. The caller mounts this
/// behind the session middleware so every handler sees the
/// optional `ValidatedSession` extension.
pub fn router(service: Arc<ApiKeyService>) -> axum::Router {
    axum::Router::new()
        .route("/", post(create_api_key).get(list_api_keys))
        .route("/{id}", axum::routing::delete(revoke_api_key))
        .route("/{id}/rotate", post(rotate_api_key))
        .with_state(service)
}

async fn create_api_key(
    State(svc): State<Arc<ApiKeyService>>,
    session: Option<Extension<ValidatedSession>>,
    Json(body): Json<CreateApiKeyRequest>,
) -> Result<(StatusCode, Json<CreateApiKeyResponse>), ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let tier = body
        .rate_limit_tier
        .unwrap_or_else(|| DEFAULT_RATE_LIMIT_TIER.to_owned());
    let issued = svc
        .issue(session.user_id, body.name, body.scopes, tier)
        .await
        .map_err(ApiError::from)?;
    // `201 Created` matches REST convention for resource
    // creation. The response body carries the cleartext exactly
    // once — the gateway logs at `debug` to confirm a key was
    // minted (with id only, NEVER cleartext) so audit needs
    // don't leak material into log storage.
    debug!(
        user_id = %session.user_id,
        key_id = %issued.id,
        "api key minted"
    );
    Ok((
        StatusCode::CREATED,
        Json(CreateApiKeyResponse {
            id: issued.id,
            cleartext: issued.cleartext,
            key_prefix: issued.key_prefix,
        }),
    ))
}

async fn list_api_keys(
    State(svc): State<Arc<ApiKeyService>>,
    session: Option<Extension<ValidatedSession>>,
) -> Result<Json<Vec<ApiKeySummary>>, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let rows = svc.list_for_user(session.user_id).await?;
    Ok(Json(rows.into_iter().map(ApiKeySummary::from).collect()))
}

async fn revoke_api_key(
    State(svc): State<Arc<ApiKeyService>>,
    session: Option<Extension<ValidatedSession>>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let revoked = svc.revoke(id, session.user_id).await?;
    match revoked {
        // 204 No Content for the success case — REST idiom for
        // a successful state-change with nothing to return.
        Some(_) => Ok(StatusCode::NO_CONTENT),
        // 404 covers both "wrong user" and "wrong id" — the
        // auth service deliberately collapses them so an
        // attacker probing for valid key ids can't distinguish.
        None => Err(ApiError::NotFound),
    }
}

async fn rotate_api_key(
    State(svc): State<Arc<ApiKeyService>>,
    session: Option<Extension<ValidatedSession>>,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<CreateApiKeyResponse>), ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let rotated = svc.rotate(id, session.user_id).await?;
    match rotated {
        Some(issued) => {
            debug!(
                user_id = %session.user_id,
                old_key_id = %id,
                new_key_id = %issued.id,
                "api key rotated"
            );
            Ok((
                StatusCode::CREATED,
                Json(CreateApiKeyResponse {
                    id: issued.id,
                    cleartext: issued.cleartext,
                    key_prefix: issued.key_prefix,
                }),
            ))
        }
        None => Err(ApiError::NotFound),
    }
}

/// Internal error type for the api-keys subrouter. Maps to
/// HTTP status codes via [`IntoResponse`]; nothing here leaks
/// internal detail to clients (the descriptive `message` is for
/// debugging during development, not for end-user UI).
#[derive(Debug)]
enum ApiError {
    Unauthenticated,
    NotFound,
    Validation(String),
    Internal(String),
}

impl From<AuthError> for ApiError {
    fn from(value: AuthError) -> Self {
        match value {
            AuthError::Validation(m) => Self::Validation(m),
            AuthError::Internal(m) => Self::Internal(m),
            other => Self::Internal(other.to_string()),
        }
    }
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: &'static str,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            // `WWW-Authenticate` would be the strictly-correct
            // companion header here, but the SvelteKit client
            // already keys behaviour off the status code, and
            // the gateway never issues a Basic / Bearer
            // challenge — so we omit it to avoid promising a
            // protocol the gateway doesn't implement.
            Self::Unauthenticated => (
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                "session cookie missing, invalid, or expired".to_owned(),
            ),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                "not_found",
                "api key not found or not owned by you".to_owned(),
            ),
            Self::Validation(m) => (StatusCode::BAD_REQUEST, "validation", m),
            Self::Internal(m) => {
                warn!(error = %m, "api-keys subrouter internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal error".to_owned(),
                )
            }
        };
        (status, Json(ErrorBody { error: code, message })).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_tiers_set_matches_migration_check() {
        // The migration's `mcp_api_keys_tier_allowed` CHECK lists
        // exactly these three values. Any drift here means a
        // request with a "valid" tier could be rejected by the
        // DB at insert time — or worse, a tier the gateway
        // accepts gets stored that the DB CHECK should have
        // rejected. Keep them in lockstep.
        assert_eq!(auth::ALLOWED_TIERS, ["free", "pro", "enterprise"]);
    }

    #[test]
    fn api_key_summary_omits_cleartext_fields() {
        // `ApiKeySummary` is the only shape `list_api_keys`
        // serialises. The compile-time set of fields must NOT
        // include anything resembling cleartext / key_hash;
        // this is a guardrail against an over-eager future
        // `#[derive(Serialize)]` on `ApiKeyRow` itself.
        let json = serde_json::to_string(&ApiKeySummary {
            id: Uuid::nil(),
            name: "t".into(),
            key_prefix: "tdh_abcd".into(),
            scopes: vec![],
            rate_limit_tier: "free".into(),
            created_at: Utc::now(),
            last_used_at: None,
            revoked_at: None,
        })
        .unwrap();
        assert!(!json.contains("cleartext"), "json={json}");
        assert!(!json.contains("key_hash"), "json={json}");
    }

    #[test]
    fn unauthenticated_error_maps_to_401() {
        let r = ApiError::Unauthenticated.into_response();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn not_found_error_maps_to_404() {
        let r = ApiError::NotFound.into_response();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn validation_error_maps_to_400() {
        let r = ApiError::Validation("bad tier".into()).into_response();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn internal_error_maps_to_500() {
        let r = ApiError::Internal("boom".into()).into_response();
        assert_eq!(r.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn create_request_defaults_tier_and_scopes() {
        let req: CreateApiKeyRequest = serde_json::from_str(r#"{"name":"laptop"}"#).unwrap();
        assert_eq!(req.name, "laptop");
        assert!(req.scopes.is_empty());
        assert!(req.rate_limit_tier.is_none());
    }
}
