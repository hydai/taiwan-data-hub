//! `etl_dlq` dead-letter repository (#5b.1).
//!
//! The retry-with-backoff envelope in `etl-worker` writes
//! one row per terminal failure — after exhausting the
//! configured attempts — so operators can read a single
//! table to find sources that need manual attention.
//! Transient failures that recovered on a later attempt
//! never reach this repo.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;
use uuid::Uuid;

use crate::{Storage, StorageError};

/// Normalised `ConnectorError` category so SQL filters
/// can target a failure mode without parsing the message.
/// Mirrors the variant names in `connectors::ConnectorError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DlqErrorKind {
    Transport,
    BadStatus,
    Decode,
    Config,
    InvalidCursor,
    Unsupported,
    Other,
}

impl DlqErrorKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Transport => "transport",
            Self::BadStatus => "bad_status",
            Self::Decode => "decode",
            Self::Config => "config",
            Self::InvalidCursor => "invalid_cursor",
            Self::Unsupported => "unsupported",
            Self::Other => "other",
        }
    }

    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "transport" => Some(Self::Transport),
            "bad_status" => Some(Self::BadStatus),
            "decode" => Some(Self::Decode),
            "config" => Some(Self::Config),
            "invalid_cursor" => Some(Self::InvalidCursor),
            "unsupported" => Some(Self::Unsupported),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewDlqEntry {
    /// `datasets.source` token — `data_gov_tw`, `twse`, etc.
    pub source: String,
    /// Operation that failed — `list_datasets`, `fetch_metadata`, …
    pub job_kind: String,
    /// How many tries the envelope made (≥ 1).
    pub attempts: i32,
    pub error_kind: DlqErrorKind,
    pub error_message: String,
    /// Optional context — cursor, HTTP status, response
    /// excerpt, etc. Connector chooses shape.
    pub payload: Option<JsonValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DlqRow {
    pub id: Uuid,
    pub source: String,
    pub job_kind: String,
    pub attempts: i32,
    pub error_kind: DlqErrorKind,
    pub error_message: String,
    pub payload: Option<JsonValue>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub resolution_note: Option<String>,
}

#[async_trait]
pub trait EtlDlqRepo: Send + Sync {
    /// Insert one terminal-failure row.
    async fn insert(&self, new: NewDlqEntry) -> Result<DlqRow, StorageError>;

    /// List open (unresolved) DLQ rows, newest first. Used
    /// by the operator dashboard. `limit` caps the page;
    /// callers paginate via repeated calls with the
    /// previous page's last `id` (cursor).
    async fn list_open(&self, after: Option<Uuid>, limit: i64)
    -> Result<Vec<DlqRow>, StorageError>;
}

#[async_trait]
impl EtlDlqRepo for Storage {
    async fn insert(&self, new: NewDlqEntry) -> Result<DlqRow, StorageError> {
        let row: (
            Uuid,
            String,
            String,
            i32,
            String,
            String,
            Option<JsonValue>,
            DateTime<Utc>,
            Option<DateTime<Utc>>,
            Option<String>,
        ) = sqlx::query_as(
            "INSERT INTO etl_dlq
                 (source, job_kind, attempts, error_kind, error_message, payload)
             VALUES ($1, $2, $3, $4, $5, $6)
             RETURNING id, source, job_kind, attempts, error_kind, error_message,
                       payload, created_at, resolved_at, resolution_note",
        )
        .bind(new.source)
        .bind(new.job_kind)
        .bind(new.attempts)
        .bind(new.error_kind.as_str())
        .bind(new.error_message)
        .bind(new.payload)
        .fetch_one(self.pool())
        .await?;
        let error_kind = DlqErrorKind::from_wire(&row.4).ok_or_else(|| {
            StorageError::Decode(format!(
                "unknown etl_dlq.error_kind {:?} (CHECK drift?)",
                row.4
            ))
        })?;
        Ok(DlqRow {
            id: row.0,
            source: row.1,
            job_kind: row.2,
            attempts: row.3,
            error_kind,
            error_message: row.5,
            payload: row.6,
            created_at: row.7,
            resolved_at: row.8,
            resolution_note: row.9,
        })
    }

    async fn list_open(
        &self,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<DlqRow>, StorageError> {
        // Newest-first stable cursor via id DESC.
        // UUIDv7 is time-ordered so id-DESC ≡ created_at-
        // DESC, but id is a strict total order: two rows
        // with the same created_at can't straddle a page
        // boundary.
        let rows: Vec<(
            Uuid,
            String,
            String,
            i32,
            String,
            String,
            Option<JsonValue>,
            DateTime<Utc>,
            Option<DateTime<Utc>>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT id, source, job_kind, attempts, error_kind, error_message,
                    payload, created_at, resolved_at, resolution_note
               FROM etl_dlq
              WHERE resolved_at IS NULL
                AND ($1::UUID IS NULL OR id < $1::UUID)
              ORDER BY id DESC
              LIMIT $2",
        )
        .bind(after)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|r| {
                let error_kind = DlqErrorKind::from_wire(&r.4).ok_or_else(|| {
                    StorageError::Decode(format!(
                        "unknown etl_dlq.error_kind {:?} (CHECK drift?)",
                        r.4
                    ))
                })?;
                Ok(DlqRow {
                    id: r.0,
                    source: r.1,
                    job_kind: r.2,
                    attempts: r.3,
                    error_kind,
                    error_message: r.5,
                    payload: r.6,
                    created_at: r.7,
                    resolved_at: r.8,
                    resolution_note: r.9,
                })
            })
            .collect()
    }
}
