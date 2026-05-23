//! `/api/v1/admin/submissions` HTTP routes (#5a.2).
//!
//! Moderator-side submission CRUD. All routes are session-
//! gated AND role-gated: a regular `user` session reaches the
//! handler but the [`auth::ModerationService::require_moderator`]
//! gate flips it to 403.
//!
//! Endpoints:
//!
//!   * `GET    /api/v1/admin/submissions` — list pending,
//!     optionally filtered with `?kind=dataset|tool|…`.
//!   * `GET    /api/v1/admin/submissions/{id}` — single row,
//!     regardless of status, so a moderator can re-open an
//!     already-decided submission for audit.
//!   * `POST   /api/v1/admin/submissions/{id}/approve` — body
//!     `{ "reason": "..." }` (reason optional on approve).
//!   * `POST   /api/v1/admin/submissions/{id}/reject` — body
//!     `{ "reason": "..." }` (reason MANDATORY on reject).
//!
//! Promotion of approved submissions into the canonical
//! `datasets` table lands separately (#5b.6 provenance
//! follow-up); this PR ships the moderation queue + audit
//! trail only.

use std::sync::Arc;

use auth::{AuthError, ModerationDenialReason, ModerationService, ValidatedSession};
use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use storage::{SubmissionKind, SubmissionRow};
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ModerationSubmission {
    pub id: Uuid,
    pub user_id: Uuid,
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

impl From<SubmissionRow> for ModerationSubmission {
    fn from(row: SubmissionRow) -> Self {
        Self {
            id: row.id,
            user_id: row.user_id,
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

#[derive(Debug, Deserialize)]
pub struct ListPendingQuery {
    /// Optional kind filter — accepts the same four wire
    /// strings as `submissions.submission_kind`.
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DecisionRequest {
    /// Free-form moderator note. Mandatory on reject, optional
    /// on approve.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DecisionResponse {
    pub submission: ModerationSubmission,
    pub audit_log_id: Uuid,
}

pub fn router(service: Arc<ModerationService>) -> axum::Router {
    let collection = get(list_pending);
    axum::Router::new()
        .route("/", collection.clone())
        .route("", collection)
        .route("/{id}", get(get_submission))
        .route("/{id}/approve", post(approve_submission))
        .route("/{id}/reject", post(reject_submission))
        .with_state(service)
}

async fn list_pending(
    State(svc): State<Arc<ModerationService>>,
    session: Option<Extension<ValidatedSession>>,
    Query(query): Query<ListPendingQuery>,
) -> Result<Json<Vec<ModerationSubmission>>, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    svc.require_moderator(session.user_id)
        .await
        .map_err(ApiError::from)?
        .map_err(ApiError::from_denial)?;
    let kind_filter = match query.kind.as_deref() {
        None | Some("") => None,
        Some(s) => Some(
            SubmissionKind::from_wire(s)
                .ok_or_else(|| ApiError::Validation(format!("unknown kind: {s}")))?,
        ),
    };
    let rows = svc
        .list_pending(kind_filter)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(
        rows.into_iter().map(ModerationSubmission::from).collect(),
    ))
}

async fn get_submission(
    State(svc): State<Arc<ModerationService>>,
    session: Option<Extension<ValidatedSession>>,
    Path(id): Path<String>,
) -> Result<Json<ModerationSubmission>, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    svc.require_moderator(session.user_id)
        .await
        .map_err(ApiError::from)?
        .map_err(ApiError::from_denial)?;
    let id = parse_submission_id(&id)?;
    let row = svc.get(id).await.map_err(ApiError::from)?;
    row.map(ModerationSubmission::from)
        .map(Json)
        .ok_or(ApiError::NotFound)
}

async fn approve_submission(
    State(svc): State<Arc<ModerationService>>,
    session: Option<Extension<ValidatedSession>>,
    Path(id): Path<String>,
    Json(body): Json<DecisionRequest>,
) -> Result<Json<DecisionResponse>, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    svc.require_moderator(session.user_id)
        .await
        .map_err(ApiError::from)?
        .map_err(ApiError::from_denial)?;
    let id = parse_submission_id(&id)?;
    let outcome = svc
        .approve(session.user_id, id, body.reason)
        .await
        .map_err(ApiError::from)?;
    match outcome {
        Ok(decision) => {
            info!(
                moderator_id = %session.user_id,
                submission_id = %id,
                audit_log_id = %decision.audit_log_id,
                "submission approved"
            );
            Ok(Json(DecisionResponse {
                submission: ModerationSubmission::from(decision.submission),
                audit_log_id: decision.audit_log_id,
            }))
        }
        Err(reason) => Err(ApiError::from_denial(reason)),
    }
}

async fn reject_submission(
    State(svc): State<Arc<ModerationService>>,
    session: Option<Extension<ValidatedSession>>,
    Path(id): Path<String>,
    Json(body): Json<DecisionRequest>,
) -> Result<Json<DecisionResponse>, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    svc.require_moderator(session.user_id)
        .await
        .map_err(ApiError::from)?
        .map_err(ApiError::from_denial)?;
    let id = parse_submission_id(&id)?;
    let reason = body.reason.unwrap_or_default();
    let outcome = svc
        .reject(session.user_id, id, reason)
        .await
        .map_err(ApiError::from)?;
    match outcome {
        Ok(decision) => {
            info!(
                moderator_id = %session.user_id,
                submission_id = %id,
                audit_log_id = %decision.audit_log_id,
                "submission rejected"
            );
            Ok(Json(DecisionResponse {
                submission: ModerationSubmission::from(decision.submission),
                audit_log_id: decision.audit_log_id,
            }))
        }
        Err(reason) => Err(ApiError::from_denial(reason)),
    }
}

fn parse_submission_id(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw)
        .map_err(|_| ApiError::Validation(format!("submission id `{raw}` is not a valid UUID")))
}

#[derive(Debug)]
enum ApiError {
    Unauthenticated,
    Forbidden,
    NotFound,
    Validation(String),
    /// Mod queue conflict — the row was already decided by
    /// another moderator between this client's list-load and
    /// decision POST, or the row never existed in a pending
    /// state. Mapped to 409 so the UI knows to refresh.
    Conflict(String),
    Internal(String),
}

impl ApiError {
    fn from_denial(denial: ModerationDenialReason) -> Self {
        match denial {
            ModerationDenialReason::Forbidden => Self::Forbidden,
            ModerationDenialReason::NotFoundOrAlreadyDecided => Self::Conflict(
                "submission not found or already decided by another moderator".to_owned(),
            ),
            ModerationDenialReason::MissingRejectReason => {
                Self::Validation("reject requires a non-empty `reason`".to_owned())
            }
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
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                "forbidden",
                "moderator role required".to_owned(),
            ),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                "not_found",
                "submission not found".to_owned(),
            ),
            Self::Validation(m) => (StatusCode::BAD_REQUEST, "validation", m),
            Self::Conflict(m) => (StatusCode::CONFLICT, "conflict", m),
            Self::Internal(m) => {
                warn!(error = %m, "moderation subrouter internal error");
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
    use storage::SubmissionStatus;

    #[test]
    fn moderation_response_omits_review_fields_when_pending() {
        let row = SubmissionRow {
            id: Uuid::nil(),
            user_id: Uuid::nil(),
            kind: SubmissionKind::Dataset,
            status: SubmissionStatus::Pending,
            title: "x".into(),
            payload: json!({"kind":"dataset"}),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            reviewed_at: None,
            reviewed_by: None,
            review_reason: None,
        };
        let json = serde_json::to_string(&ModerationSubmission::from(row)).unwrap();
        assert!(!json.contains("reviewed_at"));
        assert!(!json.contains("reviewed_by"));
        assert!(!json.contains("review_reason"));
    }

    #[test]
    fn denial_maps_to_status_codes() {
        let conflict =
            ApiError::from_denial(ModerationDenialReason::NotFoundOrAlreadyDecided).into_response();
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        let forbidden = ApiError::from_denial(ModerationDenialReason::Forbidden).into_response();
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
        let missing =
            ApiError::from_denial(ModerationDenialReason::MissingRejectReason).into_response();
        assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
    }
}
