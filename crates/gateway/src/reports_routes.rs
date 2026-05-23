//! `/api/v1/reports` + `/api/v1/admin/reports` HTTP routes
//! (#5a.6). Three layers of permission:
//!
//!   * `POST /api/v1/reports` — any signed-in user can
//!     file a report.
//!   * `GET /api/v1/reports/mine` — caller sees their own
//!     filed reports + resolution status.
//!   * `GET /api/v1/admin/reports`,
//!     `POST /api/v1/admin/reports/:id/resolve` —
//!     moderator-gated via
//!     [`ModerationService::require_moderator`].

use std::sync::Arc;

use auth::{
    AuthError, ModerationDenialReason, ModerationService, REPORT_BODY_MAX_LEN, ReportDenialReason,
    ReportService, ResolveDenialReason, ValidatedSession,
};
use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use storage::{ReportAction, ReportReason, ReportRow, ReportTargetKind};
use tracing::warn;
use uuid::Uuid;

/// State carried into the reports router. Bundles both
/// services so the admin endpoints can role-gate via
/// `ModerationService` while still dispatching the
/// report-side mutation through `ReportService`.
#[derive(Clone)]
pub struct ReportsState {
    pub reports: Arc<ReportService>,
    pub moderation: Arc<ModerationService>,
}

#[derive(Debug, Serialize)]
pub struct ReportResponse {
    pub id: Uuid,
    // Skip serialising when None so the wire shape is
    // consistent with the other optional fields. Reporter
    // FK is SET NULL on user deletion, so a moderator
    // could see a report row whose `reporter_id` is
    // missing — clients should treat absent and null
    // identically.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reporter_id: Option<Uuid>,
    pub target_kind: &'static str,
    pub target_id: Uuid,
    pub reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_taken: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_note: Option<String>,
}

impl From<ReportRow> for ReportResponse {
    fn from(row: ReportRow) -> Self {
        Self {
            id: row.id,
            reporter_id: row.reporter_id,
            target_kind: row.target_kind.as_str(),
            target_id: row.target_id,
            reason: row.reason.as_str(),
            body: row.body,
            created_at: row.created_at,
            resolved_at: row.resolved_at,
            resolved_by: row.resolved_by,
            action_taken: row.action_taken.map(ReportAction::as_str),
            resolution_note: row.resolution_note,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SubmitResponse {
    pub id: Uuid,
    /// `true` for a fresh report row, `false` when the
    /// upsert hit an existing one (handler maps to 200
    /// in the existing-row case, 201 in the
    /// fresh-insert case).
    pub created: bool,
    pub reporter_count: i64,
    pub freshly_hidden: bool,
}

#[derive(Debug, Deserialize)]
pub struct SubmitRequest {
    #[serde(default)]
    pub target_kind: Option<String>,
    #[serde(default)]
    pub target_id: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListOpenQuery {
    /// String-typed so axum's query extractor never
    /// rejects a malformed timestamp with a plain-text
    /// 400 — the parse happens in the handler so a bad
    /// `?before=` returns the structured `{error,
    /// message}` envelope every other endpoint uses.
    #[serde(default)]
    pub before: Option<String>,
    /// Same shape as `before` — string-typed so a
    /// non-integer `?limit=` doesn't bypass the
    /// envelope.
    #[serde(default)]
    pub limit: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResolveRequest {
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub resolution_note: Option<String>,
}

const DEFAULT_LIST_LIMIT: i64 = 50;
const MAX_LIST_LIMIT: i64 = 200;

/// Public router (signed-in user): file reports + read
/// own report history.
pub fn user_router(state: ReportsState) -> axum::Router {
    let collection = post(submit_report);
    axum::Router::new()
        .route("/", collection.clone())
        .route("", collection)
        .route("/mine", get(list_mine))
        .with_state(state)
}

/// Admin router (moderator only): queue + dispositioning.
pub fn admin_router(state: ReportsState) -> axum::Router {
    axum::Router::new()
        .route("/", get(list_open))
        .route("", get(list_open))
        .route("/{id}/resolve", post(resolve_report))
        .with_state(state)
}

async fn submit_report(
    State(state): State<ReportsState>,
    session: Option<Extension<ValidatedSession>>,
    Json(body): Json<SubmitRequest>,
) -> Result<(StatusCode, Json<SubmitResponse>), ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let kind_str = body
        .target_kind
        .as_deref()
        .ok_or_else(|| ApiError::Validation("target_kind is required".to_owned()))?;
    let id_str = body
        .target_id
        .as_deref()
        .ok_or_else(|| ApiError::Validation("target_id is required".to_owned()))?;
    let reason_str = body
        .reason
        .as_deref()
        .ok_or_else(|| ApiError::Validation("reason is required".to_owned()))?;
    let target_kind = parse_target_kind(kind_str)?;
    let target_id = parse_uuid("target_id", id_str)?;
    let reason = parse_reason(reason_str)?;
    let outcome = state
        .reports
        .submit(session.user_id, target_kind, target_id, reason, body.body)
        .await
        .map_err(ApiError::from)?;
    match outcome {
        Ok(o) => {
            // 201 only when a fresh row was inserted; the
            // upsert's existing-row path returns 200 since
            // nothing was created at the wire level.
            let status = if o.created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            Ok((
                status,
                Json(SubmitResponse {
                    id: o.report_id,
                    created: o.created,
                    reporter_count: o.reporter_count,
                    freshly_hidden: o.freshly_hidden,
                }),
            ))
        }
        Err(ReportDenialReason::BodyTooLong) => Err(ApiError::Validation(format!(
            "body too long (max {REPORT_BODY_MAX_LEN} characters)"
        ))),
        Err(ReportDenialReason::TargetNotFound) => Err(ApiError::NotFound),
    }
}

async fn list_mine(
    State(state): State<ReportsState>,
    session: Option<Extension<ValidatedSession>>,
    Query(query): Query<ListOpenQuery>,
) -> Result<Json<Vec<ReportResponse>>, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    let limit = parse_limit(query.limit.as_deref())?;
    let rows = state
        .reports
        .list_for_reporter(session.user_id, limit)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(rows.into_iter().map(ReportResponse::from).collect()))
}

async fn list_open(
    State(state): State<ReportsState>,
    session: Option<Extension<ValidatedSession>>,
    Query(query): Query<ListOpenQuery>,
) -> Result<Json<Vec<ReportResponse>>, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    state
        .moderation
        .require_moderator(session.user_id)
        .await
        .map_err(ApiError::from)?
        .map_err(ApiError::from_moderation_denial)?;
    let limit = parse_limit(query.limit.as_deref())?;
    let before = parse_before(query.before.as_deref())?;
    let rows = state
        .reports
        .list_open(before, limit)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(rows.into_iter().map(ReportResponse::from).collect()))
}

async fn resolve_report(
    State(state): State<ReportsState>,
    session: Option<Extension<ValidatedSession>>,
    Path(id): Path<String>,
    Json(body): Json<ResolveRequest>,
) -> Result<Json<ReportResponse>, ApiError> {
    let session = session.ok_or(ApiError::Unauthenticated)?.0;
    state
        .moderation
        .require_moderator(session.user_id)
        .await
        .map_err(ApiError::from)?
        .map_err(ApiError::from_moderation_denial)?;
    let id = parse_uuid("report id", &id)?;
    let action_str = body
        .action
        .as_deref()
        .ok_or_else(|| ApiError::Validation("action is required".to_owned()))?;
    let action = parse_action(action_str)?;
    let outcome = state
        .reports
        .resolve(id, session.user_id, action, body.resolution_note)
        .await
        .map_err(ApiError::from)?;
    match outcome {
        Ok(row) => Ok(Json(ReportResponse::from(row))),
        Err(ResolveDenialReason::NotFoundOrResolved) => Err(ApiError::NotFound),
        Err(ResolveDenialReason::CannotDeleteSubmission) => Err(ApiError::Validation(
            "submissions cannot be deleted via the report queue".to_owned(),
        )),
    }
}

fn parse_target_kind(raw: &str) -> Result<ReportTargetKind, ApiError> {
    ReportTargetKind::from_wire(raw)
        .ok_or_else(|| ApiError::Validation(format!("unknown target_kind: {raw}")))
}

fn parse_reason(raw: &str) -> Result<ReportReason, ApiError> {
    ReportReason::from_wire(raw)
        .ok_or_else(|| ApiError::Validation(format!("unknown reason: {raw}")))
}

fn parse_action(raw: &str) -> Result<ReportAction, ApiError> {
    ReportAction::from_wire(raw)
        .ok_or_else(|| ApiError::Validation(format!("unknown action: {raw}")))
}

fn parse_uuid(field: &str, raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw)
        .map_err(|_| ApiError::Validation(format!("{field} `{raw}` is not a valid UUID")))
}

/// Parse `?limit=` into an i64.
///
/// * Missing / blank / non-positive (`<= 0`) → falls
///   back to `DEFAULT_LIST_LIMIT`. The "non-positive
///   = default" path matches what client UIs expect
///   when they send `?limit=0` to mean "page size
///   please".
/// * Positive value → clamped to `MAX_LIST_LIMIT`.
/// * Non-integer string → `ApiError::Validation` so
///   the response keeps the `{error, message}`
///   envelope (raw `Option<i64>` on the query
///   extractor would let axum reject with a
///   plain-text 400 instead).
fn parse_limit(raw: Option<&str>) -> Result<i64, ApiError> {
    let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(DEFAULT_LIST_LIMIT);
    };
    let v: i64 = s
        .parse()
        .map_err(|_| ApiError::Validation(format!("limit `{s}` is not a valid integer")))?;
    Ok(if v <= 0 {
        DEFAULT_LIST_LIMIT
    } else {
        v.min(MAX_LIST_LIMIT)
    })
}

/// Parse `?before=` as an RFC3339 timestamp; same
/// envelope-preserving rationale as `parse_limit`.
fn parse_before(raw: Option<&str>) -> Result<Option<DateTime<Utc>>, ApiError> {
    let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let parsed: DateTime<Utc> = s.parse().map_err(|_| {
        ApiError::Validation(format!("before `{s}` is not a valid RFC3339 timestamp"))
    })?;
    Ok(Some(parsed))
}

#[derive(Debug)]
enum ApiError {
    Unauthenticated,
    Forbidden,
    NotFound,
    Validation(String),
    Internal(String),
}

impl ApiError {
    fn from_moderation_denial(reason: ModerationDenialReason) -> Self {
        // Today `require_moderator` only ever returns
        // `Forbidden`. Mapping the whole enum to
        // `Self::Forbidden` keeps a future enum extension
        // from silently 500-ing if a new variant lands —
        // we'd see it in a code review when the
        // `Forbidden` mapping looks too generous, not
        // first at a customer's 500 page.
        let _ = reason;
        Self::Forbidden
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
                "report not found or already resolved".to_owned(),
            ),
            Self::Validation(m) => (StatusCode::BAD_REQUEST, "validation", m),
            Self::Internal(m) => {
                warn!(error = %m, "reports subrouter internal error");
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
    fn parse_target_kind_known() {
        assert!(parse_target_kind("comment").is_ok());
        assert!(parse_target_kind("submission").is_ok());
    }

    #[test]
    fn parse_target_kind_rejects_unknown() {
        assert!(matches!(
            parse_target_kind("dataset").unwrap_err(),
            ApiError::Validation(_)
        ));
    }

    #[test]
    fn parse_reason_known() {
        for r in &[
            "spam",
            "harassment",
            "off_topic",
            "illegal",
            "inaccurate",
            "other",
        ] {
            assert!(parse_reason(r).is_ok(), "reason {r} must parse");
        }
    }

    #[test]
    fn parse_action_known() {
        for a in &["hide", "keep", "delete", "warn_author"] {
            assert!(parse_action(a).is_ok(), "action {a} must parse");
        }
    }

    #[test]
    fn parse_limit_defaults_and_caps() {
        assert_eq!(parse_limit(None).unwrap(), DEFAULT_LIST_LIMIT);
        assert_eq!(parse_limit(Some("")).unwrap(), DEFAULT_LIST_LIMIT);
        assert_eq!(parse_limit(Some("0")).unwrap(), DEFAULT_LIST_LIMIT);
        assert_eq!(parse_limit(Some("10")).unwrap(), 10);
        assert_eq!(parse_limit(Some("1000")).unwrap(), MAX_LIST_LIMIT);
    }

    #[test]
    fn parse_limit_rejects_garbage() {
        assert!(matches!(
            parse_limit(Some("not-a-number")).unwrap_err(),
            ApiError::Validation(_)
        ));
    }

    #[test]
    fn parse_before_accepts_rfc3339() {
        assert!(
            parse_before(Some("2026-05-23T10:00:00Z"))
                .unwrap()
                .is_some()
        );
        assert!(parse_before(None).unwrap().is_none());
        assert!(parse_before(Some("")).unwrap().is_none());
    }

    #[test]
    fn parse_before_rejects_garbage() {
        assert!(matches!(
            parse_before(Some("not-a-date")).unwrap_err(),
            ApiError::Validation(_)
        ));
    }
}
