//! `/api/v1/comments` HTTP routes (#5a.3).
//!
//! Surface:
//!
//!   * `GET    /api/v1/comments?target_kind=…&target_id=…`
//!     — list every comment on a target (public; soft-deleted
//!     rows surface as tombstones).
//!   * `POST   /api/v1/comments` — session-required. Body
//!     `{ target_kind, target_id, parent_id?, body_md }`.
//!   * `PATCH  /api/v1/comments/{id}` — session-required.
//!     Body `{ body_md }`. Edit window enforced server-side.
//!   * `DELETE /api/v1/comments/{id}` — session-required.
//!     Soft-delete (tombstone).
//!
//! The list endpoint is intentionally session-aware (no gate)
//! so a logged-out reader can see the thread; the write
//! endpoints require a session.

use std::sync::Arc;

use auth::{AuthError, BodyError, CommentDenialReason, CommentService, ValidatedSession};
use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use storage::CommentTargetKind;
use tracing::{debug, warn};
use uuid::Uuid;

/// Public JSON shape for a rendered comment.
#[derive(Debug, Serialize)]
pub struct CommentResponse {
    pub id: Uuid,
    pub target_kind: &'static str,
    pub target_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<Uuid>,
    pub depth: i16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_md: Option<String>,
    pub body_html: String,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<DateTime<Utc>>,
    pub is_deleted: bool,
}

impl From<auth::RenderedComment> for CommentResponse {
    fn from(c: auth::RenderedComment) -> Self {
        Self {
            id: c.id,
            target_kind: c.target_kind.as_str(),
            target_id: c.target_id,
            parent_id: c.parent_id,
            user_id: c.user_id,
            depth: c.depth,
            body_md: c.body_md,
            body_html: c.body_html,
            created_at: c.created_at,
            edited_at: c.edited_at,
            deleted_at: c.deleted_at,
            is_deleted: c.is_deleted,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub target_kind: String,
    pub target_id: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateRequest {
    pub target_kind: String,
    pub target_id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub body_md: String,
}

#[derive(Debug, Deserialize)]
pub struct EditRequest {
    pub body_md: String,
}

pub fn router(service: Arc<CommentService>) -> axum::Router {
    let collection = get(list_comments).post(create_comment);
    axum::Router::new()
        .route("/", collection.clone())
        .route("", collection)
        .route("/{id}", patch(edit_comment).delete(delete_comment))
        .with_state(service)
}

async fn list_comments(
    State(svc): State<Arc<CommentService>>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<CommentResponse>>, ApiError> {
    let target_kind = parse_kind(&query.target_kind)?;
    let target_id = parse_uuid("target_id", &query.target_id)?;
    let rendered = svc
        .list_for_target(target_kind, target_id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(
        rendered.into_iter().map(CommentResponse::from).collect(),
    ))
}

async fn create_comment(
    State(svc): State<Arc<CommentService>>,
    session: Option<Extension<ValidatedSession>>,
    Json(body): Json<CreateRequest>,
) -> Result<(StatusCode, Json<CommentResponse>), ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let target_kind = parse_kind(&body.target_kind)?;
    let target_id = parse_uuid("target_id", &body.target_id)?;
    let parent_id = match body.parent_id.as_deref() {
        None | Some("") => None,
        Some(s) => Some(parse_uuid("parent_id", s)?),
    };
    let outcome = svc
        .create(
            session.user_id,
            target_kind,
            target_id,
            parent_id,
            body.body_md,
        )
        .await
        .map_err(ApiError::from)?;
    match outcome {
        Ok(rendered) => {
            debug!(
                user_id = %session.user_id,
                comment_id = %rendered.id,
                "comment created"
            );
            Ok((StatusCode::CREATED, Json(CommentResponse::from(rendered))))
        }
        Err(reason) => Err(ApiError::from_denial(reason)),
    }
}

async fn edit_comment(
    State(svc): State<Arc<CommentService>>,
    session: Option<Extension<ValidatedSession>>,
    Path(id): Path<String>,
    Json(body): Json<EditRequest>,
) -> Result<Json<CommentResponse>, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let id = parse_uuid("comment id", &id)?;
    let outcome = svc
        .edit(session.user_id, id, body.body_md)
        .await
        .map_err(ApiError::from)?;
    match outcome {
        Ok(rendered) => Ok(Json(CommentResponse::from(rendered))),
        Err(reason) => Err(ApiError::from_denial(reason)),
    }
}

async fn delete_comment(
    State(svc): State<Arc<CommentService>>,
    session: Option<Extension<ValidatedSession>>,
    Path(id): Path<String>,
) -> Result<Json<CommentResponse>, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let id = parse_uuid("comment id", &id)?;
    let outcome = svc
        .delete(session.user_id, id)
        .await
        .map_err(ApiError::from)?;
    match outcome {
        Ok(rendered) => Ok(Json(CommentResponse::from(rendered))),
        Err(reason) => Err(ApiError::from_denial(reason)),
    }
}

fn parse_kind(raw: &str) -> Result<CommentTargetKind, ApiError> {
    CommentTargetKind::from_wire(raw)
        .ok_or_else(|| ApiError::Validation(format!("unknown target_kind: {raw}")))
}

fn parse_uuid(field: &str, raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw)
        .map_err(|_| ApiError::Validation(format!("{field} `{raw}` is not a valid UUID")))
}

#[derive(Debug)]
enum ApiError {
    Unauthenticated,
    NotFound,
    Validation(String),
    Conflict(String),
    Internal(String),
}

impl ApiError {
    fn from_denial(denial: CommentDenialReason) -> Self {
        match denial {
            CommentDenialReason::NotFoundOrNotYours => Self::NotFound,
            CommentDenialReason::EditWindowClosed => {
                Self::Conflict("edit window has closed (5 minutes after posting)".to_owned())
            }
            CommentDenialReason::DepthCapExceeded => {
                Self::Validation("replies cannot be nested more than one level".to_owned())
            }
            CommentDenialReason::ParentNotFound => Self::Validation(
                "parent_id does not exist or belongs to a different target".to_owned(),
            ),
            CommentDenialReason::InvalidBody(BodyError::Empty) => {
                Self::Validation("body_md cannot be empty".to_owned())
            }
            CommentDenialReason::InvalidBody(BodyError::TooLong) => Self::Validation(format!(
                "body_md too long (max {} characters)",
                auth::MAX_COMMENT_BODY_LEN
            )),
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
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                "not_found",
                "comment not found or not owned by you".to_owned(),
            ),
            Self::Validation(m) => (StatusCode::BAD_REQUEST, "validation", m),
            Self::Conflict(m) => (StatusCode::CONFLICT, "conflict", m),
            Self::Internal(m) => {
                warn!(error = %m, "comments subrouter internal error");
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
