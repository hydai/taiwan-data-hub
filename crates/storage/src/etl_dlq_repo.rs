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
///
/// The first six variants mirror `connectors::ConnectorError`
/// one-for-one (the worker's retry classifier produces them
/// via `dlq_error_kind`). `Other` is a writer-side bucket
/// for cases the classifier doesn't otherwise cover —
/// extending the enum still requires lockstep updates in
/// the CHECK constraint AND `from_wire`, so any value
/// reaching `from_wire` that doesn't match a variant is a
/// CHECK-drift bug (handled as `StorageError::Decode`,
/// loud rather than silent).
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
    /// Coarse operation tag identifying the unit of work
    /// that failed. The worker writes `crawl_pass` today
    /// (wrapping the whole `run_one_pass`); future per-
    /// dataset envelopes (`fetch_metadata` / `fetch_data`)
    /// will pick their own tags. Free-form by design —
    /// adding a new tag doesn't require a schema migration.
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

    /// List open (unresolved) DLQ rows, newest first.
    /// Used by the operator dashboard. `limit` caps the
    /// page; callers paginate via repeated calls with the
    /// previous page's last `id` as `before`.
    ///
    /// Parameter is named `before` (not `after`) because
    /// the query is `id < $before ORDER BY id DESC` —
    /// each call returns rows OLDER than the cursor. This
    /// deliberately contrasts with `ReportRepo::list_open`,
    /// which uses `after` for an ASC walk; keeping the
    /// parameter names aligned with the actual direction
    /// prevents a caller from accidentally composing the
    /// two cursors backwards.
    async fn list_open(
        &self,
        before: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<DlqRow>, StorageError>;
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
        before: Option<Uuid>,
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
        .bind(before)
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;
    use testcontainers_modules::postgres::Postgres as PgContainer;
    use testcontainers_modules::testcontainers::ContainerAsync;
    use testcontainers_modules::testcontainers::ImageExt;
    use testcontainers_modules::testcontainers::runners::AsyncRunner;

    /// Spin up a Postgres container, run every migration, and
    /// return a [`Storage`] pointed at it. Mirrors the helper
    /// in `comment_repo::tests` / `dataset_repo::tests`.
    async fn fresh_storage() -> (Storage, ContainerAsync<PgContainer>) {
        let container = PgContainer::default()
            .with_tag("18-alpine")
            .start()
            .await
            .expect("start postgres container");
        let host = container.get_host().await.expect("host");
        let port = container.get_host_port_ipv4(5432).await.expect("port");
        let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("connect");
        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .expect("migrate");
        (Storage::from_pool(pool), container)
    }

    fn sample_entry(source: &str, error_kind: DlqErrorKind) -> NewDlqEntry {
        NewDlqEntry {
            source: source.into(),
            job_kind: "crawl_pass".into(),
            attempts: 3,
            error_kind,
            error_message: "upstream returned 503".into(),
            payload: Some(serde_json::json!({ "status": 503 })),
        }
    }

    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn insert_then_list_roundtrips() {
        let (storage, _c) = fresh_storage().await;
        let inserted = storage
            .insert(sample_entry("data_gov_tw", DlqErrorKind::BadStatus))
            .await
            .expect("insert");
        assert_eq!(inserted.source, "data_gov_tw");
        assert_eq!(inserted.job_kind, "crawl_pass");
        assert_eq!(inserted.attempts, 3);
        assert_eq!(inserted.error_kind, DlqErrorKind::BadStatus);
        assert!(inserted.resolved_at.is_none());
        let listed = storage.list_open(None, 10).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, inserted.id);
        assert_eq!(listed[0].error_kind, DlqErrorKind::BadStatus);
    }

    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn list_open_paginates_newest_first_via_before_cursor() {
        let (storage, _c) = fresh_storage().await;
        // Insert three rows in order; UUIDv7 ids are strictly
        // time-ordered so `id DESC` ≡ insertion-DESC.
        let a = storage
            .insert(sample_entry("data_gov_tw", DlqErrorKind::BadStatus))
            .await
            .unwrap();
        let b = storage
            .insert(sample_entry("twse", DlqErrorKind::Transport))
            .await
            .unwrap();
        let c = storage
            .insert(sample_entry("moea", DlqErrorKind::Decode))
            .await
            .unwrap();
        // Page 1: newest first, no cursor.
        let page1 = storage.list_open(None, 2).await.unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].id, c.id, "newest first");
        assert_eq!(page1[1].id, b.id);
        // Page 2: cursor is the last id on page 1. Should
        // return the older row.
        let page2 = storage.list_open(Some(page1[1].id), 2).await.unwrap();
        assert_eq!(page2.len(), 1);
        assert_eq!(page2[0].id, a.id, "older than cursor");
    }

    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn list_open_skips_resolved_rows() {
        let (storage, _c) = fresh_storage().await;
        let open = storage
            .insert(sample_entry("data_gov_tw", DlqErrorKind::BadStatus))
            .await
            .unwrap();
        let to_resolve = storage
            .insert(sample_entry("twse", DlqErrorKind::Transport))
            .await
            .unwrap();
        // Mark one row resolved by hand (the resolve UI lands
        // in a later milestone; this exercises the partial
        // index's predicate).
        sqlx::query("UPDATE etl_dlq SET resolved_at = now() WHERE id = $1")
            .bind(to_resolve.id)
            .execute(storage.pool())
            .await
            .unwrap();
        let listed = storage.list_open(None, 10).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, open.id);
    }
}
