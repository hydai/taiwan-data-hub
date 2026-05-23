//! `/api/v1/submissions` HTTP routes (#5a.1).
//!
//! Author-side submission CRUD. All routes are session-gated:
//! the upstream [`crate::session_middleware`] either injects a
//! [`auth::ValidatedSession`] extension on a valid cookie or
//! leaves it absent. Handlers extract via `Option<…>` so
//! they can return `401` (rather than letting axum 500 on a
//! missing extractor) when the cookie was missing / invalid /
//! revoked / expired — mirrors the api-keys subrouter.
//!
//! Endpoints:
//!
//!   * `POST   /api/v1/submissions`            — create a new
//!     submission in `status='pending'`. Body is a tagged
//!     [`auth::SubmissionPayload`] JSON object.
//!   * `GET    /api/v1/submissions`            — list every
//!     submission the caller has authored, newest first.
//!   * `GET    /api/v1/submissions/{id}`       — single row,
//!     ownership-scoped (someone else's draft 404s).
//!   * `DELETE /api/v1/submissions/{id}`       — author-side
//!     withdraw. Only `pending` rows are withdrawable; an
//!     already-decided / already-withdrawn row 404s.
//!
//! Moderator-side endpoints (`PATCH …/decision`) ship with
//! #5a.2.

use std::sync::Arc;

use auth::{AuthError, SubmissionPayload, SubmissionService, ValidatedSession};
use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use storage::{SubmissionRow, SubmissionStatus};
use tracing::{debug, warn};
use uuid::Uuid;

/// JSON response shape for a single submission. Mirrors the
/// storage row 1:1 with two adjustments:
///
///   * `kind` and `status` are emitted as their wire strings
///     (matching the JSONB discriminator) rather than the Rust
///     enum.
///   * The decision triple (`reviewed_at` / `reviewed_by` /
///     `review_reason`) is omitted when the row is still
///     `pending`, so the response shape never carries a
///     dangling `null` for the common case.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct SubmissionResponse {
    pub id: Uuid,
    pub kind: &'static str,
    pub status: &'static str,
    pub title: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewed_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewed_by: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_reason: Option<String>,
}

impl From<SubmissionRow> for SubmissionResponse {
    fn from(row: SubmissionRow) -> Self {
        Self {
            id: row.id,
            kind: row.kind.as_str(),
            status: row.status.as_str(),
            title: row.title,
            payload: row.payload,
            created_at: row.created_at,
            updated_at: row.updated_at,
            reviewed_at: row.reviewed_at,
            reviewed_by: row.reviewed_by,
            review_reason: row.review_reason,
        }
    }
}

/// JSON response for the create endpoint. The `id` field is the
/// row's UUID; the `SvelteKit` form uses it to redirect to the
/// "my submissions / detail" view immediately after submit.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct CreateSubmissionResponse {
    pub id: Uuid,
    pub status: &'static str,
}

/// Build the `/api/v1/submissions` subrouter. The caller mounts
/// this behind the session middleware so every handler sees the
/// optional `ValidatedSession` extension.
///
/// The collection route is registered on BOTH `""` and `"/"`
/// so callers reach the same handler whether they hit
/// `/api/v1/submissions` (`SvelteKit`'s chosen URL) or
/// `/api/v1/submissions/` (a path some clients add a trailing
/// slash to automatically) — matches the api-keys pattern.
pub fn router(service: Arc<SubmissionService>) -> axum::Router {
    let collection = post(create_submission).get(list_submissions);
    axum::Router::new()
        .route("/", collection.clone())
        .route("", collection)
        .route("/{id}", get(get_submission).delete(withdraw_submission))
        .with_state(service)
}

async fn create_submission(
    State(svc): State<Arc<SubmissionService>>,
    session: Option<Extension<ValidatedSession>>,
    Json(payload): Json<SubmissionPayload>,
) -> Result<(StatusCode, Json<CreateSubmissionResponse>), ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let id = svc
        .create(session.user_id, payload)
        .await
        .map_err(ApiError::from)?;
    debug!(
        user_id = %session.user_id,
        submission_id = %id,
        "submission created"
    );
    Ok((
        StatusCode::CREATED,
        Json(CreateSubmissionResponse {
            id,
            status: SubmissionStatus::Pending.as_str(),
        }),
    ))
}

async fn list_submissions(
    State(svc): State<Arc<SubmissionService>>,
    session: Option<Extension<ValidatedSession>>,
) -> Result<Json<Vec<SubmissionResponse>>, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let rows = svc
        .list_for_user(session.user_id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(
        rows.into_iter().map(SubmissionResponse::from).collect(),
    ))
}

async fn get_submission(
    State(svc): State<Arc<SubmissionService>>,
    session: Option<Extension<ValidatedSession>>,
    Path(id): Path<String>,
) -> Result<Json<SubmissionResponse>, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let id = parse_submission_id(&id)?;
    let row = svc
        .get_for_user(id, session.user_id)
        .await
        .map_err(ApiError::from)?;
    row.map(SubmissionResponse::from)
        .map(Json)
        .ok_or(ApiError::NotFound)
}

async fn withdraw_submission(
    State(svc): State<Arc<SubmissionService>>,
    session: Option<Extension<ValidatedSession>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let id = parse_submission_id(&id)?;
    let updated = svc
        .withdraw(id, session.user_id)
        .await
        .map_err(ApiError::from)?;
    match updated {
        Some(_) => Ok(StatusCode::NO_CONTENT),
        // 404 covers "wrong user", "wrong id", and
        // "already-terminal status". The service deliberately
        // collapses them so an attacker probing for valid IDs
        // can't distinguish.
        None => Err(ApiError::NotFound),
    }
}

/// Parse the `{id}` path segment into a [`Uuid`] and map a
/// failure to [`ApiError::Validation`] — see `api_keys_routes`
/// for the rationale behind extracting `Path<String>` rather
/// than `Path<Uuid>`.
fn parse_submission_id(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw)
        .map_err(|_| ApiError::Validation(format!("submission id `{raw}` is not a valid UUID")))
}

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
            Self::Unauthenticated => (
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                "session cookie missing, invalid, or expired".to_owned(),
            ),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                "not_found",
                "submission not found or not owned by you".to_owned(),
            ),
            Self::Validation(m) => (StatusCode::BAD_REQUEST, "validation", m),
            Self::Internal(m) => {
                warn!(error = %m, "submissions subrouter internal error");
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
    use serde_json::json;
    use storage::SubmissionKind;

    #[test]
    fn submission_response_hides_review_fields_when_pending() {
        let row = SubmissionRow {
            id: Uuid::nil(),
            user_id: Uuid::nil(),
            kind: SubmissionKind::Dataset,
            status: SubmissionStatus::Pending,
            title: "x".into(),
            payload: json!({"kind": "dataset"}),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            reviewed_at: None,
            reviewed_by: None,
            review_reason: None,
        };
        let json = serde_json::to_string(&SubmissionResponse::from(row)).unwrap();
        // Three keys are deliberately omitted on a pending row
        // because all three carry `Option::is_none` skipper.
        assert!(!json.contains("reviewed_at"));
        assert!(!json.contains("reviewed_by"));
        assert!(!json.contains("review_reason"));
        assert!(json.contains("\"status\":\"pending\""));
        assert!(json.contains("\"kind\":\"dataset\""));
    }

    #[test]
    fn create_response_carries_initial_status() {
        let body = CreateSubmissionResponse {
            id: Uuid::nil(),
            status: SubmissionStatus::Pending.as_str(),
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(json.contains("\"status\":\"pending\""));
    }

    #[test]
    fn parse_submission_id_rejects_garbage() {
        let err = parse_submission_id("not-a-uuid").unwrap_err();
        assert!(matches!(err, ApiError::Validation(_)));
    }
}
