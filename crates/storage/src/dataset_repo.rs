//! `datasets` table repository.

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use connectors::{DatasetMetadata, SourceId};
use serde_json::{Map, Value};
use sqlx::{FromRow, Pool, Postgres, postgres::PgPoolOptions};
use uuid::Uuid;

/// Object-safe read trait so tool implementations (#1.6, #1.7, ...)
/// can be unit-tested with an in-memory stub rather than depending
/// on a live Postgres pool. [`Storage`] is the production impl.
#[async_trait]
pub trait DatasetReader: Send + Sync + 'static {
    async fn get_dataset(&self, key: DatasetKey) -> Result<Option<DatasetFull>, StorageError>;
}

#[async_trait]
impl DatasetReader for Storage {
    async fn get_dataset(&self, key: DatasetKey) -> Result<Option<DatasetFull>, StorageError> {
        Storage::get_dataset(self, key).await
    }
}

/// Object-safe lookup for the `query_rows` MCP tool (#1.7). Returns
/// just enough to find the cached Parquet for a dataset (or tell the
/// caller to materialise it first).
#[async_trait]
pub trait DatasetCacheLookup: Send + Sync + 'static {
    async fn dataset_cache(&self, key: DatasetKey)
    -> Result<Option<DatasetCacheRef>, StorageError>;
}

#[async_trait]
impl DatasetCacheLookup for Storage {
    async fn dataset_cache(
        &self,
        key: DatasetKey,
    ) -> Result<Option<DatasetCacheRef>, StorageError> {
        Storage::dataset_cache(self, key).await
    }
}

/// What `query_rows` needs to know about a dataset to either run a
/// query or tell the user to materialise it first.
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct DatasetCacheRef {
    pub id: Uuid,
    pub slug: String,
    /// `true` iff the latest version has been written to local /
    /// object cache and `cache_path` is non-null.
    pub cached: bool,
    /// Storage URI (`file:// `, `s3://`, …) for the cached Parquet.
    /// `None` until #1.8 materialises it.
    pub cache_path: Option<String>,
}

/// Object-safe write trait used by the ETL driver. Mirrors the
/// [`DatasetReader`] pattern so `run_one_pass` can be unit-tested with
/// an in-memory stub instead of a real Postgres pool. [`Storage`] is
/// the production impl.
#[async_trait]
pub trait DatasetWriter: Send + Sync + 'static {
    /// See [`Storage::upsert_dataset`].
    async fn upsert_dataset(
        &self,
        domain_id: i16,
        source: SourceId,
        metadata: &DatasetMetadata,
    ) -> Result<Uuid, StorageError>;

    /// See [`Storage::domain_id_for_slug`].
    async fn domain_id_for_slug(&self, slug: &str) -> Result<Option<i16>, StorageError>;
}

#[async_trait]
impl DatasetWriter for Storage {
    async fn upsert_dataset(
        &self,
        domain_id: i16,
        source: SourceId,
        metadata: &DatasetMetadata,
    ) -> Result<Uuid, StorageError> {
        Storage::upsert_dataset(self, domain_id, source, metadata).await
    }

    async fn domain_id_for_slug(&self, slug: &str) -> Result<Option<i16>, StorageError> {
        Storage::domain_id_for_slug(self, slug).await
    }
}

/// Object-safe search trait so the `search_datasets` MCP tool can be
/// unit-tested with an in-memory stub. Mirrors [`DatasetReader`] and
/// [`DatasetWriter`].
#[async_trait]
pub trait DatasetSearcher: Send + Sync + 'static {
    /// Returns hits ordered by relevance (when `q` is set) then by
    /// `last_modified_at` descending. Caller sets the locale used to
    /// resolve `title` / `description`; the storage layer always
    /// falls back to `zh-TW` when the requested locale is absent.
    async fn search_datasets(&self, params: SearchParams) -> Result<SearchPage, StorageError>;
}

#[async_trait]
impl DatasetSearcher for Storage {
    async fn search_datasets(&self, params: SearchParams) -> Result<SearchPage, StorageError> {
        Storage::search_datasets(self, params).await
    }
}

/// Filters + paging knobs for [`DatasetSearcher::search_datasets`].
///
/// All filter fields are `Option<String>` so a caller threading
/// "no constraint" through doesn't have to remember any sentinel.
/// `limit` is clamped storage-side (see [`Self::sanitise`]) to keep a
/// pathological caller from asking for `u32::MAX` rows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchParams {
    /// Free-text query. Combined with tier/domain/license via AND.
    /// Empty string is treated as "no query" so a caller doesn't
    /// have to distinguish unset vs. blank submission.
    pub q: Option<String>,
    /// `domains.slug` exact match.
    pub domain: Option<String>,
    /// `datasets.tier` exact match (platinum / gold / silver / bronze).
    pub tier: Option<String>,
    /// `datasets.license` exact match (license string is opaque).
    pub license: Option<String>,
    /// Locale used to render `title` / `description` in each hit.
    /// `None` defaults to `zh-TW` per CLAUDE.md's fallback chain.
    pub locale: Option<String>,
    /// Max rows to return. Clamped to [1, [`Self::MAX_LIMIT`]] at
    /// the storage layer so a pathological caller can't drag the
    /// pool into a 10⁹-row scan.
    pub limit: u32,
    /// Rows to skip for pagination.
    pub offset: u32,
}

impl SearchParams {
    /// Per DESIGN.md §9 (#1.5 DoD): "limit ≤ 100".
    pub const MAX_LIMIT: u32 = 100;
    /// Default `limit` when the caller doesn't supply one.
    pub const DEFAULT_LIMIT: u32 = 20;

    /// Apply storage-side guard rails: clamp `limit` into
    /// `[1, MAX_LIMIT]`, blank-treat-as-unset `q`. Idempotent.
    fn sanitise(mut self) -> Self {
        let limit = if self.limit == 0 {
            Self::DEFAULT_LIMIT
        } else {
            self.limit
        };
        self.limit = limit.min(Self::MAX_LIMIT);
        // An empty / whitespace `q` is equivalent to "no full-text
        // filter"; otherwise the SQL would try to `plainto_tsquery('')`
        // which returns an empty tsquery that matches nothing — much
        // worse than returning every dataset to a casual browser.
        self.q = self.q.and_then(|raw| {
            let trimmed = raw.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        });
        self
    }
}

/// One page of [`SearchHit`]s plus the cursor needed for the next.
///
/// `total` is `None` for now — counting against tsv/trigram filters
/// is the most expensive part of FTS queries, and the MCP tool
/// surface only needs `next_offset` for "load more". A future
/// enhancement can add an opt-in `?with_total=true` knob.
#[derive(Debug, Clone)]
pub struct SearchPage {
    pub hits: Vec<SearchHit>,
    /// Offset to pass for the next call, if any. `None` when fewer
    /// than `limit` rows came back (we've reached the end).
    pub next_offset: Option<u32>,
}

/// One row of a search result, already i18n-resolved per the
/// requested locale. Returns a flat shape (no nested JSONB) so the
/// MCP tool can serialise it without further work.
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct SearchHit {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    pub description: Option<String>,
    pub domain_slug: String,
    pub tier: String,
    pub license: String,
    pub publisher: Option<String>,
}

/// Lookup key for [`Storage::get_dataset`].
#[derive(Debug, Clone)]
pub enum DatasetKey {
    /// UUID primary key.
    Id(Uuid),
    /// Marketplace slug (`datasets.slug`). Slugs are unique per
    /// dataset; the storage layer takes the slug at face value
    /// without checking uniqueness across sources.
    Slug(String),
}

impl DatasetKey {
    pub fn id(id: Uuid) -> Self {
        Self::Id(id)
    }
    pub fn slug(slug: impl Into<String>) -> Self {
        Self::Slug(slug.into())
    }
}

/// One row from the `datasets` table, untouched by i18n resolution —
/// callers (tool layer) decide which locale to render. JSONB columns
/// are returned as `serde_json::Value` so tests can assert against
/// shapes without re-binding the schema in Rust types.
#[derive(Debug, Clone, FromRow)]
pub struct DatasetRow {
    pub id: Uuid,
    pub source: String,
    pub source_id: String,
    pub slug: String,
    pub domain_id: i16,
    pub title_i18n: Value,
    pub description_i18n: Option<Value>,
    pub tier: String,
    pub license: String,
    pub publisher: Option<String>,
    pub update_frequency: Option<String>,
    pub original_url: Option<String>,
    pub schema_json: Option<Value>,
    pub row_count_estimate: Option<i64>,
    pub last_modified_at: DateTime<Utc>,
    pub first_seen_at: DateTime<Utc>,
}

/// One row from `dataset_versions`.
#[derive(Debug, Clone, FromRow)]
pub struct DatasetVersionRow {
    pub id: Uuid,
    pub dataset_id: Uuid,
    pub version: String,
    pub fetched_at: DateTime<Utc>,
    pub checksum: Option<String>,
    pub row_count: Option<i64>,
    pub schema_diff: Option<Value>,
}

/// One row from `dataset_files`.
#[derive(Debug, Clone, FromRow)]
pub struct DatasetFileRow {
    pub id: Uuid,
    pub dataset_version_id: Uuid,
    pub format: String,
    pub uri: String,
    pub byte_size: Option<i64>,
    pub checksum: Option<String>,
}

/// A version with its file children attached. Versions stream out
/// newest-first; files within a version are ordered by format then id
/// for stable rendering.
#[derive(Debug, Clone)]
pub struct VersionWithFiles {
    pub version: DatasetVersionRow,
    pub files: Vec<DatasetFileRow>,
}

/// Complete read view of a dataset — the shape `get_dataset`
/// (MCP tool #1.6) is built on top of.
#[derive(Debug, Clone)]
pub struct DatasetFull {
    pub dataset: DatasetRow,
    pub versions: Vec<VersionWithFiles>,
}

/// `PostgreSQL` pool wrapper. `Clone` is cheap — the inner pool is
/// `Arc`-backed.
#[derive(Debug, Clone)]
pub struct Storage {
    pool: Pool<Postgres>,
}

impl Storage {
    /// Connect to Postgres at `database_url`. The pool is sized for the
    /// gateway's expected fan-out (5 connections), not for a heavy ETL
    /// crawl — the crawler should construct its own [`Storage`] with a
    /// larger pool via [`Self::from_pool`] once #1.4c lands.
    pub async fn connect(database_url: &str) -> Result<Self, StorageError> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }

    /// Wrap an existing pool — used by tests (testcontainers) and by
    /// callers that need to share a pool across crates.
    pub fn from_pool(pool: Pool<Postgres>) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &Pool<Postgres> {
        &self.pool
    }

    /// Insert or update the `datasets` row matching `(source,
    /// source_id)`. Returns the row's UUID — newly minted on insert,
    /// preserved on update.
    ///
    /// Upstream-controlled columns are refreshed on every call.
    /// Internal columns — `tier`, `tier_score`, `tier_override`,
    /// `cached`, `cache_path`, `tsv`, `first_seen_at` — are left
    /// untouched on update so the tier classifier and FTS pipeline
    /// can run independently of the crawl cadence.
    pub async fn upsert_dataset(
        &self,
        domain_id: i16,
        source: SourceId,
        metadata: &DatasetMetadata,
    ) -> Result<Uuid, StorageError> {
        let title = i18n_to_jsonb(&metadata.title_i18n)?;
        let description = if metadata.description_i18n.is_empty() {
            None
        } else {
            Some(i18n_to_jsonb(&metadata.description_i18n)?)
        };

        let row: (Uuid,) = sqlx::query_as(UPSERT_SQL)
            .bind(source.as_str())
            .bind(&metadata.source_id)
            .bind(&metadata.slug)
            .bind(domain_id)
            .bind(title)
            .bind(description)
            .bind(&metadata.license)
            .bind(metadata.publisher.as_ref())
            .bind(metadata.update_frequency.as_ref())
            .bind(metadata.original_url.as_ref())
            // `Option<DateTime<Utc>>` is bound as NULL when upstream
            // omits the timestamp. The SQL COALESCE chain then picks
            // the right fallback for each path (insert vs. update).
            .bind(metadata.last_modified_at)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0)
    }

    /// Resolve a domain slug to its surrogate `domain_id`. Returns
    /// `None` when no row matches — caller decides what to do
    /// (skip the dataset, default to a fallback bucket, etc.).
    /// The `domains` table is tiny (20 rows) and changes rarely;
    /// callers that need to look up many slugs in one crawl should
    /// cache the result themselves.
    pub async fn domain_id_for_slug(&self, slug: &str) -> Result<Option<i16>, StorageError> {
        let row: Option<(i16,)> = sqlx::query_as("SELECT id FROM domains WHERE slug = $1")
            .bind(slug)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.0))
    }

    /// Fetch the full read view for a dataset by id or slug. Returns
    /// `None` when no row matches; the tool layer translates that to
    /// the MCP "not found" error.
    ///
    /// Runs three round-trips (datasets / versions / files) rather
    /// than one big JOIN-aggregating query because the read fan-out
    /// is small (a dataset has tens of versions max, each with a
    /// handful of files) and three planning-friendly queries beat
    /// a single mega-statement that grows with every column added
    /// to the schema.
    ///
    /// Wrapped in a `REPEATABLE READ` transaction so the three
    /// statements observe one snapshot. Postgres's default
    /// `READ COMMITTED` takes a new snapshot per statement, which
    /// would let `dataset_versions` and `dataset_files` reflect
    /// commits that landed between the initial dataset lookup and
    /// the join — including a file row whose parent version was
    /// already dropped.
    pub async fn get_dataset(&self, key: DatasetKey) -> Result<Option<DatasetFull>, StorageError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await?;

        let dataset: Option<DatasetRow> = match &key {
            DatasetKey::Id(id) => {
                sqlx::query_as::<_, DatasetRow>(DATASET_BY_ID_SQL)
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await?
            }
            DatasetKey::Slug(slug) => {
                sqlx::query_as::<_, DatasetRow>(DATASET_BY_SLUG_SQL)
                    .bind(slug)
                    .fetch_optional(&mut *tx)
                    .await?
            }
        };

        let Some(dataset) = dataset else {
            tx.commit().await?;
            return Ok(None);
        };

        let versions: Vec<DatasetVersionRow> = sqlx::query_as(VERSIONS_BY_DATASET_SQL)
            .bind(dataset.id)
            .fetch_all(&mut *tx)
            .await?;

        let version_ids: Vec<Uuid> = versions.iter().map(|v| v.id).collect();
        let files: Vec<DatasetFileRow> = if version_ids.is_empty() {
            Vec::new()
        } else {
            sqlx::query_as(FILES_BY_VERSION_IDS_SQL)
                .bind(&version_ids)
                .fetch_all(&mut *tx)
                .await?
        };

        tx.commit().await?;

        // Group files under their parent version while preserving the
        // version order (newest first). A linear scan suffices for
        // realistic version counts.
        let mut versions_with_files: Vec<VersionWithFiles> = versions
            .into_iter()
            .map(|v| VersionWithFiles {
                version: v,
                files: Vec::new(),
            })
            .collect();
        for f in files {
            if let Some(slot) = versions_with_files
                .iter_mut()
                .find(|vwf| vwf.version.id == f.dataset_version_id)
            {
                slot.files.push(f);
            }
        }

        Ok(Some(DatasetFull {
            dataset,
            versions: versions_with_files,
        }))
    }

    /// Look up the minimal dataset info `query_rows` needs:
    /// id + slug for error messaging plus the cache state. Returns
    /// `None` when no dataset matches the key (the tool layer
    /// translates to `NotFound`); returns `Some(_)` with
    /// `cached = false / cache_path = None` when the dataset exists
    /// but hasn't been materialised yet (the tool layer translates
    /// to "call `materialize_dataset` first").
    pub async fn dataset_cache(
        &self,
        key: DatasetKey,
    ) -> Result<Option<DatasetCacheRef>, StorageError> {
        match key {
            DatasetKey::Id(id) => {
                sqlx::query_as("SELECT id, slug, cached, cache_path FROM datasets WHERE id = $1")
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(StorageError::from)
            }
            DatasetKey::Slug(slug) => {
                sqlx::query_as("SELECT id, slug, cached, cache_path FROM datasets WHERE slug = $1")
                    .bind(slug)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(StorageError::from)
            }
        }
    }

    /// Search the dataset catalog. `q` runs against the FTS `tsv`
    /// column (populated by the `datasets_tsv_trigger`) **OR** the
    /// trigram-indexed `searchable_text` column — combining them
    /// gives English keyword search + CJK substring search without
    /// requiring zhparser. The OR is fine for the planner: either
    /// branch has a GIN index it can pick, and the row counts are
    /// small enough that the dedup cost is irrelevant.
    ///
    /// Returns hits ordered by tsv rank (when `q` is set) then by
    /// `last_modified_at` descending. The locale fallback chain is
    /// requested-locale → `zh-TW`.
    ///
    /// `limit` is clamped to [`SearchParams::MAX_LIMIT`] inside
    /// [`SearchParams::sanitise`] so a careless caller can't blow up
    /// the connection.
    pub async fn search_datasets(&self, params: SearchParams) -> Result<SearchPage, StorageError> {
        let params = params.sanitise();
        let locale = params.locale.as_deref().unwrap_or("zh-TW");

        // Fetch `limit + 1` so we can tell whether there's another
        // page without a separate COUNT(*) — the +1 row is dropped
        // before returning to the caller.
        let fetch = i64::from(params.limit) + 1;
        let offset = i64::from(params.offset);

        // `q` is bound TWICE: raw for `plainto_tsquery` (no LIKE
        // semantics), and LIKE-escaped for the trigram-backed ILIKE
        // branch. Without escaping, a `q` of `"_"` becomes a wildcard
        // that matches every row with ≥ 1 character in
        // `searchable_text`, and `"100%"` matches "100" + anything.
        // Parameter binding stops SQL injection but doesn't make
        // LIKE metacharacters literal — that's our job.
        let q_escaped = params.q.as_deref().map(escape_like_pattern);

        let hits: Vec<SearchHit> = sqlx::query_as(SEARCH_DATASETS_SQL)
            .bind(params.q.as_deref())
            .bind(q_escaped.as_deref())
            .bind(params.domain.as_deref())
            .bind(params.tier.as_deref())
            .bind(params.license.as_deref())
            .bind(locale)
            .bind(fetch)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?;

        // Compare in `usize` space (`u32 as usize` widens losslessly on
        // 32-bit and 64-bit targets; the inverse `hits.len() as i64`
        // can wrap on 64-bit and trips `clippy::cast_possible_wrap`).
        let limit_usize = params.limit as usize;
        let has_more = hits.len() > limit_usize;
        let hits: Vec<SearchHit> = hits.into_iter().take(limit_usize).collect();
        let next_offset = has_more.then(|| params.offset.saturating_add(params.limit));

        Ok(SearchPage { hits, next_offset })
    }
}

/// Multi-condition search. Each filter is `NULL`-skipped via the
/// `$N::text IS NULL OR ...` idiom so one prepared statement covers
/// every shape — no Rust-side string concatenation, no SQL injection
/// surface. The FTS branch combines `tsv @@ ...` (works for English
/// thanks to the `simple` config) with `searchable_text ILIKE '%q%'`
/// (works for CJK thanks to the `pg_trgm` GIN index from
/// `0004_datasets_search.sql`); the planner picks whichever index
/// hits.
///
/// Bind layout:
///   $1 — raw q (or NULL)        ← `plainto_tsquery` accepts it as-is
///   $2 — LIKE-escaped q (or NULL) ← prevents `%`/`_` wildcarding
///   $3 — domain slug filter
///   $4 — tier filter
///   $5 — license filter
///   $6 — locale (always set)
///   $7 — fetch count (limit + 1)
///   $8 — row offset
const SEARCH_DATASETS_SQL: &str = "\
SELECT \
    d.id, \
    d.slug, \
    coalesce(d.title_i18n->>$6::text, d.title_i18n->>'zh-TW') AS title, \
    coalesce(d.description_i18n->>$6::text, d.description_i18n->>'zh-TW') AS description, \
    dom.slug AS domain_slug, \
    d.tier, \
    d.license, \
    d.publisher \
FROM datasets d \
JOIN domains dom ON dom.id = d.domain_id \
WHERE \
    ($1::text IS NULL OR \
        d.tsv @@ plainto_tsquery('simple', $1::text) \
        OR d.searchable_text ILIKE '%' || $2::text || '%') \
    AND ($3::text IS NULL OR dom.slug = $3::text) \
    AND ($4::text IS NULL OR d.tier = $4::text) \
    AND ($5::text IS NULL OR d.license = $5::text) \
ORDER BY \
    CASE WHEN $1::text IS NULL THEN 0.0::real \
         ELSE ts_rank(d.tsv, plainto_tsquery('simple', $1::text)) \
    END DESC, \
    d.last_modified_at DESC \
LIMIT $7 OFFSET $8\
";

/// Escape LIKE/ILIKE metacharacters in `pattern` so a query string
/// behaves as a literal substring match rather than a wildcard.
/// Postgres's default LIKE escape character is `\` (see
/// `LIKE ... ESCAPE '\'`), so we use `\\`, `\%`, `\_` as the escape
/// sequences and don't need to add an `ESCAPE` clause to the SQL.
fn escape_like_pattern(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Convert a `BTreeMap<String, String>` into the `jsonb` shape the
/// schema CHECK constraints expect (`{"zh-TW": "...", "en": "..."}`).
/// Returns [`StorageError::MissingZhTw`] when the source language is
/// absent — the schema would reject it loudly anyway, but failing
/// here keeps the error surface aligned with the connector contract.
fn i18n_to_jsonb(text: &BTreeMap<String, String>) -> Result<Value, StorageError> {
    if !text.contains_key("zh-TW") {
        return Err(StorageError::MissingZhTw);
    }
    let mut map = Map::with_capacity(text.len());
    for (k, v) in text {
        map.insert(k.clone(), Value::String(v.clone()));
    }
    Ok(Value::Object(map))
}

/// Errors emitted by the storage layer. `sqlx::Error` covers
/// connection/transport/SQL-level issues; [`Self::MissingZhTw`]
/// is a domain-level invariant check.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("i18n payload missing required `zh-TW` key")]
    MissingZhTw,
    #[error("JSON encoding error: {0}")]
    Json(#[from] serde_json::Error),
}

// Read-side SQL — column lists are explicit (rather than `SELECT *`)
// so row decoding stays stable against future schema additions; any
// new field added to `DatasetRow` must be added here in lockstep, and
// the testcontainers tests catch the drift at `cargo test`.

const DATASET_BY_ID_SQL: &str = "
    SELECT
        id, source, source_id, slug, domain_id,
        title_i18n, description_i18n, tier, license, publisher,
        update_frequency, original_url, schema_json, row_count_estimate,
        last_modified_at, first_seen_at
    FROM datasets
    WHERE id = $1
";

const DATASET_BY_SLUG_SQL: &str = "
    SELECT
        id, source, source_id, slug, domain_id,
        title_i18n, description_i18n, tier, license, publisher,
        update_frequency, original_url, schema_json, row_count_estimate,
        last_modified_at, first_seen_at
    FROM datasets
    WHERE slug = $1
";

const VERSIONS_BY_DATASET_SQL: &str = "
    SELECT id, dataset_id, version, fetched_at, checksum, row_count, schema_diff
    FROM dataset_versions
    WHERE dataset_id = $1
    ORDER BY fetched_at DESC, id
";

const FILES_BY_VERSION_IDS_SQL: &str = "
    SELECT id, dataset_version_id, format, uri, byte_size, checksum
    FROM dataset_files
    WHERE dataset_version_id = ANY($1)
    ORDER BY format, id
";

/// ON CONFLICT preserves the internal tier / cache columns and the
/// `first_seen_at` timestamp; only upstream-controlled columns refresh.
/// The `id` is not modified by the conflict clause, so the row's UUID
/// stays stable across updates and `RETURNING id` gives callers a key
/// they can use to correlate later writes. (`UUIDv7`'s sortability is
/// a separate property — it orders rows by creation time but is not
/// what guarantees update-stability.)
///
/// `last_modified_at` uses `COALESCE($11, ...)` to keep the meaning
/// of "upstream omitted the timestamp" honest across both code paths:
///
/// - INSERT — fall back to `now()` (the column's `NOT NULL` default).
/// - UPDATE — preserve the existing value (`datasets.last_modified_at`).
///   Without this, every crawl of a source that doesn't carry
///   `metadata_modified` would bump the timestamp on every run.
const UPSERT_SQL: &str = "
    INSERT INTO datasets (
        source, source_id, slug, domain_id,
        title_i18n, description_i18n,
        license, publisher, update_frequency, original_url,
        last_modified_at
    ) VALUES (
        $1, $2, $3, $4,
        $5, $6,
        $7, $8, $9, $10,
        COALESCE($11, now())
    )
    ON CONFLICT (source, source_id) DO UPDATE SET
        slug              = EXCLUDED.slug,
        domain_id         = EXCLUDED.domain_id,
        title_i18n        = EXCLUDED.title_i18n,
        description_i18n  = EXCLUDED.description_i18n,
        license           = EXCLUDED.license,
        publisher         = EXCLUDED.publisher,
        update_frequency  = EXCLUDED.update_frequency,
        original_url      = EXCLUDED.original_url,
        last_modified_at  = COALESCE($11, datasets.last_modified_at)
    RETURNING id;
";

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use testcontainers_modules::postgres::Postgres as PgContainer;
    use testcontainers_modules::testcontainers::ContainerAsync;
    use testcontainers_modules::testcontainers::ImageExt;
    use testcontainers_modules::testcontainers::runners::AsyncRunner;

    /// Spin up a fresh Postgres 18 container, run the project's
    /// migrations against it, and return a [`Storage`] pointed at
    /// it. The container is held alive by the returned handle —
    /// drop it to terminate the container.
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

    fn sample_metadata() -> DatasetMetadata {
        DatasetMetadata {
            source_id: "11102".into(),
            slug: "real-estate-prices".into(),
            title_i18n: BTreeMap::from([
                ("zh-TW".to_owned(), "實價登錄價格".to_owned()),
                ("en".to_owned(), "Real estate prices".to_owned()),
            ]),
            description_i18n: BTreeMap::from([(
                "zh-TW".to_owned(),
                "全國不動產交易實價揭露".to_owned(),
            )]),
            license: "政府資料開放授權條款-第1版".into(),
            publisher: Some("內政部地政司".into()),
            update_frequency: Some("每月更新".into()),
            original_url: Some("https://data.gov.tw/dataset/real-estate-prices".into()),
            last_modified_at: DateTime::parse_from_rfc3339("2026-04-15T03:30:00Z")
                .ok()
                .map(|d| d.with_timezone(&Utc)),
            upstream_categories: vec!["不動產與土地".into()],
        }
    }

    /// realestate-land domain is seeded with id=1 in
    /// `migrations/0002_seed_domains.sql` (first INSERT row).
    async fn realestate_land_id(storage: &Storage) -> i16 {
        let row: (i16,) = sqlx::query_as("SELECT id FROM domains WHERE slug = 'realestate-land'")
            .fetch_one(storage.pool())
            .await
            .expect("seeded domain present");
        row.0
    }

    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn upsert_inserts_then_updates_on_natural_key() {
        let (storage, _container) = fresh_storage().await;
        let domain_id = realestate_land_id(&storage).await;

        let m1 = sample_metadata();
        let id_first = storage
            .upsert_dataset(domain_id, SourceId::DataGovTw, &m1)
            .await
            .expect("first upsert");

        // Same natural key, different mutable fields. UUID must
        // survive the update (UUIDv7 stays stable for the row); the
        // changed columns must be reflected.
        let mut m2 = sample_metadata();
        m2.license = "CC-BY-4.0".into();
        m2.publisher = Some("地政司".into());
        let id_second = storage
            .upsert_dataset(domain_id, SourceId::DataGovTw, &m2)
            .await
            .expect("second upsert");

        assert_eq!(id_first, id_second, "natural-key upsert must preserve id");

        let row: (String, String, Option<String>) = sqlx::query_as(
            "SELECT license, slug, publisher FROM datasets WHERE source = $1 AND source_id = $2",
        )
        .bind(SourceId::DataGovTw.as_str())
        .bind(&m1.source_id)
        .fetch_one(storage.pool())
        .await
        .expect("row present after update");
        assert_eq!(row.0, "CC-BY-4.0", "license must reflect the update");
        assert_eq!(row.1, "real-estate-prices");
        assert_eq!(
            row.2.as_deref(),
            Some("地政司"),
            "publisher must reflect the update"
        );
    }

    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn upsert_preserves_tier_default_on_first_insert() {
        let (storage, _container) = fresh_storage().await;
        let domain_id = realestate_land_id(&storage).await;

        storage
            .upsert_dataset(domain_id, SourceId::DataGovTw, &sample_metadata())
            .await
            .expect("insert");

        // `tier_score` is NUMERIC(4,3); reading it back as a typed
        // value would need the `bigdecimal` feature, so cast to text
        // on the SQL side and assert against the canonical render.
        let row: (String, String) =
            sqlx::query_as("SELECT tier, tier_score::text FROM datasets WHERE source_id = $1")
                .bind("11102")
                .fetch_one(storage.pool())
                .await
                .expect("row");
        assert_eq!(row.0, "bronze", "default tier on first insert");
        assert_eq!(row.1, "0.000");
    }

    /// Regression for Copilot PR #95 round 1: when upstream omits
    /// `metadata_modified`, an update must NOT bump `last_modified_at`
    /// to `now()`. The COALESCE in `UPSERT_SQL` should preserve the
    /// previous value.
    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn upsert_preserves_last_modified_at_when_upstream_omits_it() {
        let (storage, _container) = fresh_storage().await;
        let domain_id = realestate_land_id(&storage).await;

        // First insert: upstream provides a timestamp. Row gets it.
        let m1 = sample_metadata();
        let expected = m1.last_modified_at.expect("sample carries a timestamp");
        storage
            .upsert_dataset(domain_id, SourceId::DataGovTw, &m1)
            .await
            .expect("insert");

        // Second upsert: upstream omits `metadata_modified`. The
        // stored value MUST stay pinned to the original timestamp;
        // bumping to `now()` would falsely advertise an update.
        let mut m2 = sample_metadata();
        m2.last_modified_at = None;
        m2.license = "CC-BY-4.0".into();
        storage
            .upsert_dataset(domain_id, SourceId::DataGovTw, &m2)
            .await
            .expect("update");

        let row: (chrono::DateTime<chrono::Utc>,) =
            sqlx::query_as("SELECT last_modified_at FROM datasets WHERE source_id = $1")
                .bind("11102")
                .fetch_one(storage.pool())
                .await
                .expect("row");
        assert_eq!(
            row.0, expected,
            "last_modified_at must not move when upstream omits the value",
        );
    }

    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn get_dataset_returns_none_when_no_match() {
        let (storage, _container) = fresh_storage().await;
        // By id — Uuid::nil() will never match a real row (and we
        // don't need the v4 feature just to mint a random one).
        let by_id = storage
            .get_dataset(DatasetKey::id(Uuid::nil()))
            .await
            .expect("ok");
        assert!(by_id.is_none(), "unknown UUID must yield None");

        // By slug
        let by_slug = storage
            .get_dataset(DatasetKey::slug("nope-not-here"))
            .await
            .expect("ok");
        assert!(by_slug.is_none(), "unknown slug must yield None");
    }

    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn get_dataset_returns_full_view_by_id_and_slug() {
        let (storage, _container) = fresh_storage().await;
        let domain_id = realestate_land_id(&storage).await;

        // Seed a dataset and synthesise a version + two file rows
        // (no version-creation API yet — that's #1.4d — so we drive
        // the schema directly).
        let dataset_id = storage
            .upsert_dataset(domain_id, SourceId::DataGovTw, &sample_metadata())
            .await
            .expect("seed dataset");

        let version_id: Uuid = sqlx::query_scalar(
            "INSERT INTO dataset_versions (dataset_id, version, checksum, row_count) \
             VALUES ($1, '2026.04', 'abc123', 1000) RETURNING id",
        )
        .bind(dataset_id)
        .fetch_one(storage.pool())
        .await
        .expect("seed version");

        sqlx::query(
            "INSERT INTO dataset_files (dataset_version_id, format, uri, byte_size) \
             VALUES ($1, 'csv', 's3://bucket/a.csv', 2048), \
                    ($1, 'parquet', 's3://bucket/a.parquet', 1024)",
        )
        .bind(version_id)
        .execute(storage.pool())
        .await
        .expect("seed files");

        // ── by id ──
        let by_id = storage
            .get_dataset(DatasetKey::id(dataset_id))
            .await
            .expect("ok")
            .expect("present");
        assert_eq!(by_id.dataset.id, dataset_id);
        assert_eq!(by_id.dataset.slug, "real-estate-prices");
        assert_eq!(by_id.dataset.tier, "bronze", "default tier on first insert");
        assert_eq!(by_id.versions.len(), 1);
        let v0 = &by_id.versions[0];
        assert_eq!(v0.version.version, "2026.04");
        assert_eq!(v0.version.checksum.as_deref(), Some("abc123"));
        assert_eq!(v0.files.len(), 2);
        // Files are ordered by format; csv comes before parquet.
        assert_eq!(v0.files[0].format, "csv");
        assert_eq!(v0.files[1].format, "parquet");

        // ── by slug ── must produce the same view ──
        let by_slug = storage
            .get_dataset(DatasetKey::slug("real-estate-prices"))
            .await
            .expect("ok")
            .expect("present");
        assert_eq!(by_slug.dataset.id, dataset_id);
        assert_eq!(by_slug.versions.len(), 1);
        assert_eq!(by_slug.versions[0].files.len(), 2);
    }

    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn get_dataset_with_no_versions_returns_empty_versions_vec() {
        let (storage, _container) = fresh_storage().await;
        let domain_id = realestate_land_id(&storage).await;
        let dataset_id = storage
            .upsert_dataset(domain_id, SourceId::DataGovTw, &sample_metadata())
            .await
            .expect("seed");

        let full = storage
            .get_dataset(DatasetKey::id(dataset_id))
            .await
            .expect("ok")
            .expect("present");
        assert!(
            full.versions.is_empty(),
            "fresh dataset has no versions yet (#1.4d will start creating them)",
        );
    }

    #[test]
    fn missing_zh_tw_in_title_i18n_fails_before_db_round_trip() {
        let mut m = sample_metadata();
        m.title_i18n.remove("zh-TW");
        let err = i18n_to_jsonb(&m.title_i18n).unwrap_err();
        assert!(matches!(err, StorageError::MissingZhTw));
    }

    /// LIKE metacharacters must be escaped or the trigram branch
    /// degenerates into a wildcard match. `%` and `_` are the famous
    /// pair; `\` is the escape character itself and must be doubled.
    /// Everything else passes through verbatim, including CJK code
    /// points and emoji.
    #[test]
    fn escape_like_pattern_escapes_only_the_three_metacharacters() {
        assert_eq!(escape_like_pattern(""), "");
        assert_eq!(escape_like_pattern("plain"), "plain");
        assert_eq!(escape_like_pattern("100%"), "100\\%");
        assert_eq!(escape_like_pattern("_"), "\\_");
        assert_eq!(escape_like_pattern("a\\b"), "a\\\\b");
        assert_eq!(escape_like_pattern("a%b_c\\d"), "a\\%b\\_c\\\\d");
        // CJK + emoji untouched.
        assert_eq!(escape_like_pattern("土地"), "土地");
        assert_eq!(escape_like_pattern("🚀"), "🚀");
    }

    /// Per DESIGN.md §9 (#1.5 DoD): "limit ≤ 100". Caller-supplied
    /// values are clamped so a tool layer that forwards an attacker-
    /// controlled limit can't drag the pool into a 10⁹-row scan.
    #[test]
    fn search_params_sanitise_clamps_limit_and_blank_q() {
        let p = SearchParams {
            q: Some("   ".into()),
            limit: 100_000,
            ..Default::default()
        }
        .sanitise();
        assert_eq!(p.q, None, "whitespace-only q is treated as unset");
        assert_eq!(p.limit, SearchParams::MAX_LIMIT);

        let p = SearchParams {
            limit: 0,
            ..Default::default()
        }
        .sanitise();
        assert_eq!(
            p.limit,
            SearchParams::DEFAULT_LIMIT,
            "limit=0 maps to default — no zero-page request",
        );

        let p = SearchParams {
            q: Some("  土地  ".into()),
            limit: 50,
            ..Default::default()
        }
        .sanitise();
        assert_eq!(p.q.as_deref(), Some("土地"), "trims around the term");
        assert_eq!(p.limit, 50);
    }

    /// Seed three datasets across two domains so the search tests
    /// can assert filter + relevance ordering without overlapping
    /// `sample_metadata()`'s slug.
    async fn seed_search_corpus(storage: &Storage) {
        let realestate = realestate_land_id(storage).await;
        let env: i16 = sqlx::query_as("SELECT id FROM domains WHERE slug = 'environment'")
            .fetch_one(storage.pool())
            .await
            .map(|r: (i16,)| r.0)
            .expect("environment seeded");

        // 1. realestate / land prices — zh-TW + en title
        let m1 = DatasetMetadata {
            source_id: "land-prices".into(),
            slug: "land-prices".into(),
            title_i18n: BTreeMap::from([
                ("zh-TW".to_owned(), "全國土地交易實價".to_owned()),
                ("en".to_owned(), "Nationwide land prices".to_owned()),
            ]),
            description_i18n: BTreeMap::from([(
                "zh-TW".to_owned(),
                "全國不動產交易實價揭露".to_owned(),
            )]),
            license: "CC-BY-4.0".into(),
            publisher: Some("內政部地政司".into()),
            update_frequency: None,
            original_url: None,
            last_modified_at: DateTime::parse_from_rfc3339("2026-05-01T00:00:00Z")
                .ok()
                .map(|d| d.with_timezone(&Utc)),
            upstream_categories: vec!["不動產與土地".into()],
        };
        storage
            .upsert_dataset(realestate, SourceId::DataGovTw, &m1)
            .await
            .expect("seed land-prices");

        // 2. environment / air quality — title carries "land" in en
        let m2 = DatasetMetadata {
            source_id: "air-quality".into(),
            slug: "air-quality".into(),
            title_i18n: BTreeMap::from([
                ("zh-TW".to_owned(), "空氣品質".to_owned()),
                ("en".to_owned(), "Air quality index".to_owned()),
            ]),
            description_i18n: BTreeMap::from([(
                "zh-TW".to_owned(),
                "環保署測站每日空氣品質".to_owned(),
            )]),
            license: "OGDL-Taiwan-v1".into(),
            publisher: Some("環境部".into()),
            update_frequency: None,
            original_url: None,
            last_modified_at: DateTime::parse_from_rfc3339("2026-05-15T00:00:00Z")
                .ok()
                .map(|d| d.with_timezone(&Utc)),
            upstream_categories: vec!["環境".into()],
        };
        storage
            .upsert_dataset(env, SourceId::DataGovTw, &m2)
            .await
            .expect("seed air-quality");

        // 3. environment / forest land — both "land" (en) and "土地" (zh-TW)
        let m3 = DatasetMetadata {
            source_id: "forest-land".into(),
            slug: "forest-land".into(),
            title_i18n: BTreeMap::from([
                ("zh-TW".to_owned(), "森林土地".to_owned()),
                ("en".to_owned(), "Forest land".to_owned()),
            ]),
            description_i18n: BTreeMap::from([(
                "zh-TW".to_owned(),
                "林務局轄管森林土地".to_owned(),
            )]),
            license: "OGDL-Taiwan-v1".into(),
            publisher: Some("林業及自然保育署".into()),
            update_frequency: None,
            original_url: None,
            last_modified_at: DateTime::parse_from_rfc3339("2026-04-01T00:00:00Z")
                .ok()
                .map(|d| d.with_timezone(&Utc)),
            upstream_categories: vec!["環境".into()],
        };
        storage
            .upsert_dataset(env, SourceId::DataGovTw, &m3)
            .await
            .expect("seed forest-land");
    }

    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn search_matches_english_via_tsv_and_chinese_via_trigram() {
        let (storage, _container) = fresh_storage().await;
        seed_search_corpus(&storage).await;

        // English keyword "land" hits via tsv on land-prices & forest-land.
        let page = storage
            .search_datasets(SearchParams {
                q: Some("land".into()),
                limit: 20,
                ..Default::default()
            })
            .await
            .expect("search");
        let slugs: Vec<_> = page.hits.iter().map(|h| h.slug.as_str()).collect();
        assert!(
            slugs.contains(&"land-prices") && slugs.contains(&"forest-land"),
            "english tsv hit: got {slugs:?}",
        );

        // Chinese substring "土地" matches the zh-TW titles via pg_trgm,
        // even though `simple` config tokenises Chinese poorly.
        let page = storage
            .search_datasets(SearchParams {
                q: Some("土地".into()),
                limit: 20,
                ..Default::default()
            })
            .await
            .expect("search zh-TW");
        let slugs: Vec<_> = page.hits.iter().map(|h| h.slug.as_str()).collect();
        assert!(
            slugs.contains(&"land-prices") && slugs.contains(&"forest-land"),
            "zh-TW trigram hit: got {slugs:?}",
        );
        assert!(
            !slugs.contains(&"air-quality"),
            "air-quality has no 土地 anywhere",
        );
    }

    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn search_filters_combine_via_and() {
        let (storage, _container) = fresh_storage().await;
        seed_search_corpus(&storage).await;

        // domain=environment narrows the corpus to air-quality + forest-land.
        let page = storage
            .search_datasets(SearchParams {
                domain: Some("environment".into()),
                limit: 20,
                ..Default::default()
            })
            .await
            .expect("search domain");
        let mut slugs: Vec<_> = page.hits.iter().map(|h| h.slug.clone()).collect();
        slugs.sort();
        assert_eq!(slugs, vec!["air-quality", "forest-land"]);

        // license filter further narrows to OGDL — knocks out the
        // realestate-land row (CC-BY-4.0). Combined with domain it
        // leaves both environment rows.
        let page = storage
            .search_datasets(SearchParams {
                license: Some("OGDL-Taiwan-v1".into()),
                domain: Some("environment".into()),
                limit: 20,
                ..Default::default()
            })
            .await
            .expect("search license + domain");
        let mut slugs: Vec<_> = page.hits.iter().map(|h| h.slug.clone()).collect();
        slugs.sort();
        assert_eq!(slugs, vec!["air-quality", "forest-land"]);
    }

    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn search_locale_resolves_with_zh_tw_fallback() {
        let (storage, _container) = fresh_storage().await;
        seed_search_corpus(&storage).await;

        // English locale renders the en title when present.
        let page = storage
            .search_datasets(SearchParams {
                domain: Some("environment".into()),
                locale: Some("en".into()),
                limit: 20,
                ..Default::default()
            })
            .await
            .expect("search en");
        let air = page
            .hits
            .iter()
            .find(|h| h.slug == "air-quality")
            .expect("air-quality present");
        assert_eq!(air.title, "Air quality index");

        // Unknown locale falls back to zh-TW.
        let page = storage
            .search_datasets(SearchParams {
                domain: Some("environment".into()),
                locale: Some("ja".into()),
                limit: 20,
                ..Default::default()
            })
            .await
            .expect("search ja");
        let air = page
            .hits
            .iter()
            .find(|h| h.slug == "air-quality")
            .expect("air-quality present");
        assert_eq!(air.title, "空氣品質", "ja missing → zh-TW fallback");
    }

    /// LIKE metacharacters in the user query must NOT act as wildcards
    /// against `searchable_text`. Without `escape_like_pattern`, `q="_"`
    /// would match every dataset (`_` is "any single character" in
    /// LIKE) and `q="100%"` would match the prefix `100`. This test
    /// pins the fix: a `_` query returns zero hits because none of the
    /// seeded titles / descriptions / publishers contains a literal
    /// underscore.
    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn search_treats_like_metacharacters_as_literals() {
        let (storage, _container) = fresh_storage().await;
        seed_search_corpus(&storage).await;

        let page = storage
            .search_datasets(SearchParams {
                q: Some("_".into()),
                limit: 20,
                ..Default::default()
            })
            .await
            .expect("search _");
        assert!(
            page.hits.is_empty(),
            "underscore must be literal, not 'any char'; got {:?}",
            page.hits.iter().map(|h| &h.slug).collect::<Vec<_>>(),
        );

        let page = storage
            .search_datasets(SearchParams {
                q: Some("%".into()),
                limit: 20,
                ..Default::default()
            })
            .await
            .expect("search %");
        assert!(
            page.hits.is_empty(),
            "percent must be literal, not 'any string'; got {:?}",
            page.hits.iter().map(|h| &h.slug).collect::<Vec<_>>(),
        );
    }

    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn search_paginates_with_next_offset() {
        let (storage, _container) = fresh_storage().await;
        seed_search_corpus(&storage).await;

        // limit=2 over 3 matching rows → page 1 yields 2 hits + next_offset.
        let page1 = storage
            .search_datasets(SearchParams {
                limit: 2,
                ..Default::default()
            })
            .await
            .expect("page 1");
        assert_eq!(page1.hits.len(), 2);
        assert_eq!(page1.next_offset, Some(2));

        // Page 2 with the cursor returns the remaining row, no more pages.
        let page2 = storage
            .search_datasets(SearchParams {
                limit: 2,
                offset: page1.next_offset.unwrap(),
                ..Default::default()
            })
            .await
            .expect("page 2");
        assert_eq!(page2.hits.len(), 1);
        assert_eq!(page2.next_offset, None);
    }
}
