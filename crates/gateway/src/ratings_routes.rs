//! `/api/v1/ratings` HTTP routes (#5a.5).
//!
//! Three endpoints:
//!
//!   * `GET  /api/v1/ratings/:kind/:id` — aggregate + the
//!     viewer's own score. Anonymous-readable.
//!   * `POST /api/v1/ratings`            — upsert. Session-
//!     gated. Body: `{ target_kind, target_id, score }`.
//!   * `DELETE /api/v1/ratings/:kind/:id` — withdraw the
//!     caller's row. Session-gated.
//!
//! Success responses return raw payloads (object or empty
//! body); only errors use the `{error, message}` envelope.

use std::sync::Arc;

use auth::{
    AuthError, RatingDenialReason, RatingService, SCORE_MAX, SCORE_MIN, ValidatedSession,
};
use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use storage::{RatingTargetKind, RatingView};
use tracing::warn;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct RatingViewResponse {
    /// `avg_score` rounded to 2 decimals server-side so the
    /// client doesn't have to decide on a format. `null`
    /// when there are no ratings yet — the frontend renders
    /// "no ratings yet" instead of an artificial "0.00".
    pub avg_score: Option<f64>,
    pub count: i32,
    pub viewer_score: Option<i16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_refreshed_at: Option<DateTime<Utc>>,
}

impl From<RatingView> for RatingViewResponse {
    fn from(view: RatingView) -> Self {
        match view.aggregate {
            Some(agg) if agg.rating_count > 0 => Self {
                // Round to 2 decimals here so the wire format
                // doesn't expose float jitter from the SQL
                // AVG. `4.2666…` → `4.27`.
                avg_score: Some((agg.avg_score * 100.0).round() / 100.0),
                count: agg.rating_count,
                viewer_score: view.viewer_score,
                last_refreshed_at: Some(agg.last_refreshed_at),
            },
            _ => Self {
                avg_score: None,
                count: 0,
                viewer_score: view.viewer_score,
                last_refreshed_at: None,
            },
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct UpsertRequest {
    #[serde(default)]
    pub target_kind: Option<String>,
    #[serde(default)]
    pub target_id: Option<String>,
    #[serde(default)]
    pub score: Option<i16>,
}

#[derive(Debug, Serialize)]
pub struct UpsertResponse {
    pub id: Uuid,
    pub target_kind: &'static str,
    pub target_id: Uuid,
    pub score: i16,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub fn ratings_router(service: Arc<RatingService>) -> axum::Router {
    let collection = post(upsert_rating);
    axum::Router::new()
        .route("/", collection.clone())
        .route("", collection)
        .route(
            "/{kind}/{target_id}",
            get(view_rating).delete(withdraw_rating),
        )
        .with_state(service)
}

async fn view_rating(
    State(svc): State<Arc<RatingService>>,
    session: Option<Extension<ValidatedSession>>,
    Path((kind, target_id)): Path<(String, String)>,
) -> Result<Json<RatingViewResponse>, ApiError> {
    let target_kind = parse_kind(&kind)?;
    let target_id = parse_uuid("target_id", &target_id)?;
    let viewer_id = session.as_ref().map(|s| s.0.user_id);
    let view = svc
        .view(target_kind, target_id, viewer_id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(view.into()))
}

async fn upsert_rating(
    State(svc): State<Arc<RatingService>>,
    session: Option<Extension<ValidatedSession>>,
    Json(body): Json<UpsertRequest>,
) -> Result<Json<UpsertResponse>, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let kind_str = body
        .target_kind
        .as_deref()
        .ok_or_else(|| ApiError::Validation("target_kind is required".to_owned()))?;
    let id_str = body
        .target_id
        .as_deref()
        .ok_or_else(|| ApiError::Validation("target_id is required".to_owned()))?;
    let score = body
        .score
        .ok_or_else(|| ApiError::Validation("score is required".to_owned()))?;
    let target_kind = parse_kind(kind_str)?;
    let target_id = parse_uuid("target_id", id_str)?;
    let outcome = svc
        .upsert(session.user_id, target_kind, target_id, score)
        .await
        .map_err(ApiError::from)?;
    match outcome {
        Ok(row) => Ok(Json(UpsertResponse {
            id: row.id,
            target_kind: row.target_kind.as_str(),
            target_id: row.target_id,
            score: row.score,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })),
        Err(reason) => Err(ApiError::from_rating_denial(reason)),
    }
}

async fn withdraw_rating(
    State(svc): State<Arc<RatingService>>,
    session: Option<Extension<ValidatedSession>>,
    Path((kind, target_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let target_kind = parse_kind(&kind)?;
    let target_id = parse_uuid("target_id", &target_id)?;
    let removed = svc
        .withdraw(session.user_id, target_kind, target_id)
        .await
        .map_err(ApiError::from)?;
    // 204 either way — withdraw is idempotent and the
    // gateway shouldn't leak whether the row existed.
    let _ = removed;
    Ok(StatusCode::NO_CONTENT)
}

fn parse_kind(raw: &str) -> Result<RatingTargetKind, ApiError> {
    RatingTargetKind::from_wire(raw)
        .ok_or_else(|| ApiError::Validation(format!("unknown target_kind: {raw}")))
}

fn parse_uuid(field: &str, raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw)
        .map_err(|_| ApiError::Validation(format!("{field} `{raw}` is not a valid UUID")))
}

#[derive(Debug)]
enum ApiError {
    Unauthenticated,
    Forbidden(&'static str),
    Validation(String),
    Internal(String),
}

impl ApiError {
    fn from_rating_denial(reason: RatingDenialReason) -> Self {
        match reason {
            RatingDenialReason::ScoreOutOfRange => Self::Validation(format!(
                "score must be between {SCORE_MIN} and {SCORE_MAX}"
            )),
            RatingDenialReason::AccountTooNew => Self::Forbidden("account_too_new"),
            // A revoked session still passes the validation
            // middleware (it's keyed on the session row),
            // but the user row may have been deleted out
            // from under it. 401 is the right reaction.
            RatingDenialReason::UnknownUser => Self::Unauthenticated,
        }
    }
}

impl From<AuthError> for ApiError {
    fn from(value: AuthError) -> Self {
        Self::Internal(value.to_string())
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
            Self::Unauthenticated => (
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                "session cookie missing, invalid, or expired".to_owned(),
            ),
            Self::Forbidden("account_too_new") => (
                StatusCode::FORBIDDEN,
                "account_too_new",
                "ratings require a 24h-old account; please come back in a bit".to_owned(),
            ),
            Self::Forbidden(other) => (StatusCode::FORBIDDEN, other, "forbidden".to_owned()),
            Self::Validation(m) => (StatusCode::BAD_REQUEST, "validation", m),
            Self::Internal(m) => {
                warn!(error = %m, "ratings subrouter internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal error".to_owned(),
                )
            }
        };
        (
            status,
            Json(ErrorBody {
                error: code,
                message,
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kind_known_strings() {
        assert!(parse_kind("dataset").is_ok());
        assert!(parse_kind("tool").is_ok());
    }

    #[test]
    fn parse_kind_rejects_unknown() {
        assert!(matches!(
            parse_kind("notebook").unwrap_err(),
            ApiError::Validation(_)
        ));
    }

    #[test]
    fn parse_uuid_rejects_garbage() {
        assert!(matches!(
            parse_uuid("target_id", "nope").unwrap_err(),
            ApiError::Validation(_)
        ));
    }

    #[test]
    fn denial_mapping_picks_distinct_codes() {
        // Pin the denial → HTTP mapping so a future enum
        // variant doesn't silently fall through to 500.
        assert!(matches!(
            ApiError::from_rating_denial(RatingDenialReason::ScoreOutOfRange),
            ApiError::Validation(_)
        ));
        assert!(matches!(
            ApiError::from_rating_denial(RatingDenialReason::AccountTooNew),
            ApiError::Forbidden("account_too_new")
        ));
        assert!(matches!(
            ApiError::from_rating_denial(RatingDenialReason::UnknownUser),
            ApiError::Unauthenticated
        ));
    }
}
