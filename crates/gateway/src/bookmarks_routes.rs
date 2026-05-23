//! `/api/v1/bookmarks` + `/api/v1/collections` HTTP routes
//! (#5a.4). All endpoints are session-gated; reads + writes
//! return the structured `{error, message}` envelope.

use std::sync::Arc;

use auth::{
    AuthError, BookmarkService, CollectionDenialReason, CollectionInputError, CollectionService,
    ValidatedSession,
};
use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use storage::{
    BookmarkRow, BookmarkTargetKind, BookmarkToggleOutcome, CollectionItemRow, CollectionRow,
};
use tracing::warn;
use uuid::Uuid;

// === Bookmarks ===

#[derive(Debug, Serialize)]
pub struct BookmarkResponse {
    pub id: Uuid,
    pub target_kind: &'static str,
    pub target_id: Uuid,
    pub created_at: DateTime<Utc>,
}

impl From<BookmarkRow> for BookmarkResponse {
    fn from(row: BookmarkRow) -> Self {
        Self {
            id: row.id,
            target_kind: row.target_kind.as_str(),
            target_id: row.target_id,
            created_at: row.created_at,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ToggleRequest {
    #[serde(default)]
    pub target_kind: Option<String>,
    #[serde(default)]
    pub target_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ToggleResponse {
    /// `"bookmarked"` (heart now on, new row) or
    /// `"removed"` (heart now off).
    pub outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct ListBookmarksQuery {
    #[serde(default)]
    pub kind: Option<String>,
}

pub fn bookmarks_router(service: Arc<BookmarkService>) -> axum::Router {
    let collection = post(toggle_bookmark).get(list_bookmarks);
    axum::Router::new()
        .route("/", collection.clone())
        .route("", collection)
        .with_state(service)
}

async fn toggle_bookmark(
    State(svc): State<Arc<BookmarkService>>,
    session: Option<Extension<ValidatedSession>>,
    Json(body): Json<ToggleRequest>,
) -> Result<Json<ToggleResponse>, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let kind_str = body
        .target_kind
        .as_deref()
        .ok_or_else(|| ApiError::Validation("target_kind is required".to_owned()))?;
    let id_str = body
        .target_id
        .as_deref()
        .ok_or_else(|| ApiError::Validation("target_id is required".to_owned()))?;
    let target_kind = parse_kind(kind_str)?;
    let target_id = parse_uuid("target_id", id_str)?;
    let outcome = svc
        .toggle(session.user_id, target_kind, target_id)
        .await
        .map_err(ApiError::from)?;
    let body = match outcome {
        BookmarkToggleOutcome::Bookmarked(id) => ToggleResponse {
            outcome: "bookmarked",
            id: Some(id),
        },
        BookmarkToggleOutcome::Removed => ToggleResponse {
            outcome: "removed",
            id: None,
        },
    };
    Ok(Json(body))
}

async fn list_bookmarks(
    State(svc): State<Arc<BookmarkService>>,
    session: Option<Extension<ValidatedSession>>,
    Query(query): Query<ListBookmarksQuery>,
) -> Result<Json<Vec<BookmarkResponse>>, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let kind_filter = match query.kind.as_deref() {
        None | Some("") => None,
        Some(s) => Some(parse_kind(s)?),
    };
    let rows = svc
        .list_for_user(session.user_id, kind_filter)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(rows.into_iter().map(BookmarkResponse::from).collect()))
}

// === Collections ===

#[derive(Debug, Serialize)]
pub struct CollectionResponse {
    pub id: Uuid,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<CollectionRow> for CollectionResponse {
    fn from(row: CollectionRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            description: row.description,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CollectionItemResponse {
    pub target_kind: &'static str,
    pub target_id: Uuid,
    pub added_at: DateTime<Utc>,
}

impl From<CollectionItemRow> for CollectionItemResponse {
    fn from(row: CollectionItemRow) -> Self {
        Self {
            target_kind: row.target_kind.as_str(),
            target_id: row.target_id,
            added_at: row.added_at,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateCollectionRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ItemRequest {
    #[serde(default)]
    pub target_kind: Option<String>,
    #[serde(default)]
    pub target_id: Option<String>,
}

pub fn collections_router(service: Arc<CollectionService>) -> axum::Router {
    let collection_routes = post(create_collection).get(list_collections);
    axum::Router::new()
        .route("/", collection_routes.clone())
        .route("", collection_routes)
        .route(
            "/{id}",
            axum::routing::patch(rename_collection).delete(delete_collection),
        )
        .route(
            "/{id}/items",
            get(list_collection_items).post(add_collection_item),
        )
        .route(
            "/{id}/items/{kind}/{target_id}",
            axum::routing::delete(remove_collection_item),
        )
        .with_state(service)
}

async fn list_collections(
    State(svc): State<Arc<CollectionService>>,
    session: Option<Extension<ValidatedSession>>,
) -> Result<Json<Vec<CollectionResponse>>, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let rows = svc
        .list_for_user(session.user_id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(
        rows.into_iter().map(CollectionResponse::from).collect(),
    ))
}

async fn create_collection(
    State(svc): State<Arc<CollectionService>>,
    session: Option<Extension<ValidatedSession>>,
    Json(body): Json<CreateCollectionRequest>,
) -> Result<(StatusCode, Json<CollectionResponse>), ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let name = body
        .name
        .ok_or_else(|| ApiError::Validation("name is required".to_owned()))?;
    let outcome = svc
        .create(session.user_id, name, body.description)
        .await
        .map_err(ApiError::from)?;
    match outcome {
        Ok(row) => Ok((StatusCode::CREATED, Json(CollectionResponse::from(row)))),
        Err(reason) => Err(ApiError::from_collection_denial(reason)),
    }
}

async fn rename_collection(
    State(svc): State<Arc<CollectionService>>,
    session: Option<Extension<ValidatedSession>>,
    Path(id): Path<String>,
    Json(body): Json<CreateCollectionRequest>,
) -> Result<Json<CollectionResponse>, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let id = parse_uuid("collection id", &id)?;
    let name = body
        .name
        .ok_or_else(|| ApiError::Validation("name is required".to_owned()))?;
    let outcome = svc
        .rename(id, session.user_id, name, body.description)
        .await
        .map_err(ApiError::from)?;
    match outcome {
        Ok(row) => Ok(Json(CollectionResponse::from(row))),
        Err(reason) => Err(ApiError::from_collection_denial(reason)),
    }
}

async fn delete_collection(
    State(svc): State<Arc<CollectionService>>,
    session: Option<Extension<ValidatedSession>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let id = parse_uuid("collection id", &id)?;
    let removed = svc
        .delete(id, session.user_id)
        .await
        .map_err(ApiError::from)?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

async fn list_collection_items(
    State(svc): State<Arc<CollectionService>>,
    session: Option<Extension<ValidatedSession>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<CollectionItemResponse>>, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let id = parse_uuid("collection id", &id)?;
    let items = svc
        .list_items(id, session.user_id)
        .await
        .map_err(ApiError::from)?;
    match items {
        Some(rows) => Ok(Json(
            rows.into_iter().map(CollectionItemResponse::from).collect(),
        )),
        None => Err(ApiError::NotFound),
    }
}

async fn add_collection_item(
    State(svc): State<Arc<CollectionService>>,
    session: Option<Extension<ValidatedSession>>,
    Path(id): Path<String>,
    Json(body): Json<ItemRequest>,
) -> Result<StatusCode, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let id = parse_uuid("collection id", &id)?;
    let kind_str = body
        .target_kind
        .as_deref()
        .ok_or_else(|| ApiError::Validation("target_kind is required".to_owned()))?;
    let target_id_str = body
        .target_id
        .as_deref()
        .ok_or_else(|| ApiError::Validation("target_id is required".to_owned()))?;
    let target_kind = parse_kind(kind_str)?;
    let target_id = parse_uuid("target_id", target_id_str)?;
    let added = svc
        .add_item(id, session.user_id, target_kind, target_id)
        .await
        .map_err(ApiError::from)?;
    if added {
        return Ok(StatusCode::CREATED);
    }
    // `added == false` could mean two things: (a) the
    // collection is not owned by the caller / doesn't
    // exist, or (b) the (kind, id) is already in the
    // collection. Re-read the collection to distinguish
    // them — owner-confirmed → 204 (idempotent), unowned
    // / missing → 404. Avoids surfacing "already saved"
    // as a misleading 404 to the client.
    if svc
        .get_for_user(id, session.user_id)
        .await
        .map_err(ApiError::from)?
        .is_some()
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

async fn remove_collection_item(
    State(svc): State<Arc<CollectionService>>,
    session: Option<Extension<ValidatedSession>>,
    Path((id, kind, target_id)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let id = parse_uuid("collection id", &id)?;
    let target_kind = parse_kind(&kind)?;
    let target_id = parse_uuid("target_id", &target_id)?;
    let removed = svc
        .remove_item(id, session.user_id, target_kind, target_id)
        .await
        .map_err(ApiError::from)?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

fn parse_kind(raw: &str) -> Result<BookmarkTargetKind, ApiError> {
    BookmarkTargetKind::from_wire(raw)
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
    fn from_collection_denial(reason: CollectionDenialReason) -> Self {
        match reason {
            CollectionDenialReason::NotFoundOrNotYours => Self::NotFound,
            CollectionDenialReason::NameTaken => {
                Self::Conflict("a collection with this name already exists".to_owned())
            }
            CollectionDenialReason::InvalidInput(CollectionInputError::NameEmpty) => {
                Self::Validation("name cannot be empty".to_owned())
            }
            CollectionDenialReason::InvalidInput(CollectionInputError::NameTooLong) => {
                Self::Validation(format!(
                    "name too long (max {} characters)",
                    auth::COLLECTION_NAME_MAX_LEN
                ))
            }
            CollectionDenialReason::InvalidInput(CollectionInputError::DescriptionTooLong) => {
                Self::Validation(format!(
                    "description too long (max {} characters)",
                    auth::COLLECTION_DESCRIPTION_MAX_LEN
                ))
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
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                "not_found",
                "resource not found or not owned by you".to_owned(),
            ),
            Self::Validation(m) => (StatusCode::BAD_REQUEST, "validation", m),
            Self::Conflict(m) => (StatusCode::CONFLICT, "conflict", m),
            Self::Internal(m) => {
                warn!(error = %m, "bookmarks subrouter internal error");
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
        assert!(parse_kind("connector").is_ok());
        assert!(parse_kind("playground").is_ok());
    }

    #[test]
    fn parse_kind_rejects_unknown() {
        let err = parse_kind("notebook").unwrap_err();
        assert!(matches!(err, ApiError::Validation(_)));
    }

    #[test]
    fn parse_uuid_rejects_garbage() {
        let err = parse_uuid("collection id", "not-a-uuid").unwrap_err();
        assert!(matches!(err, ApiError::Validation(_)));
    }

    #[test]
    fn parse_uuid_accepts_valid() {
        let u = Uuid::now_v7().to_string();
        assert!(parse_uuid("collection id", &u).is_ok());
    }

    #[tokio::test]
    async fn denial_mapping_picks_distinct_codes() {
        // Each denial reason routes to a specific HTTP status
        // — pin the mapping here so a future enum tweak that
        // forgets to update the match arm is caught at CI.
        let cases = [
            (
                ApiError::from_collection_denial(CollectionDenialReason::NotFoundOrNotYours),
                StatusCode::NOT_FOUND,
            ),
            (
                ApiError::from_collection_denial(CollectionDenialReason::NameTaken),
                StatusCode::CONFLICT,
            ),
            (
                ApiError::from_collection_denial(CollectionDenialReason::InvalidInput(
                    CollectionInputError::NameEmpty,
                )),
                StatusCode::BAD_REQUEST,
            ),
            (
                ApiError::from_collection_denial(CollectionDenialReason::InvalidInput(
                    CollectionInputError::NameTooLong,
                )),
                StatusCode::BAD_REQUEST,
            ),
            (
                ApiError::from_collection_denial(CollectionDenialReason::InvalidInput(
                    CollectionInputError::DescriptionTooLong,
                )),
                StatusCode::BAD_REQUEST,
            ),
            (ApiError::Unauthenticated, StatusCode::UNAUTHORIZED),
            (ApiError::NotFound, StatusCode::NOT_FOUND),
            (ApiError::Conflict("x".into()), StatusCode::CONFLICT),
            (ApiError::Validation("x".into()), StatusCode::BAD_REQUEST),
            (
                ApiError::Internal("x".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (err, expected) in cases {
            let response = err.into_response();
            assert_eq!(response.status(), expected, "for {expected:?}");
        }
    }
}
