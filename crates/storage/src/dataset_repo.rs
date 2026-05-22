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

/// Per-source HTTP cache state for #1.4d.2 conditional fetch.
/// One row per upstream source in the `source_http_state` table.
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct SourceHttpState {
    pub source: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub last_seen_at: DateTime<Utc>,
}

/// Object-safe lookup for the `query_rows` MCP tool (#1.7). Returns
/// just enough to find the cached Parquet for a dataset (or tell the
/// caller to materialise it first).
#[async_trait]
pub trait DatasetCacheLookup: Send + Sync + 'static {
    async fn dataset_cache(&self, key: DatasetKey) -> Result<Option<CacheRef>, StorageError>;
}

#[async_trait]
impl DatasetCacheLookup for Storage {
    async fn dataset_cache(&self, key: DatasetKey) -> Result<Option<CacheRef>, StorageError> {
        Storage::dataset_cache(self, key).await
    }
}

/// What `query_rows` needs to know about a dataset to either run a
/// query or tell the user to materialise it first.
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct CacheRef {
    pub id: Uuid,
    pub slug: String,
    /// `true` iff the latest version has been written to local /
    /// object cache and `cache_path` is non-null.
    pub cached: bool,
    /// Storage URI (`file://`, `s3://`, …) for the cached Parquet.
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

    /// See [`Storage::record_version_if_changed`].
    async fn record_version_if_changed(
        &self,
        dataset_id: Uuid,
        version: &str,
        checksum: &str,
    ) -> Result<Option<Uuid>, StorageError>;

    /// See [`Storage::get_source_state`].
    async fn get_source_state(
        &self,
        source: SourceId,
    ) -> Result<Option<SourceHttpState>, StorageError>;

    /// See [`Storage::put_source_state`].
    async fn put_source_state(
        &self,
        source: SourceId,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<(), StorageError>;
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

    async fn record_version_if_changed(
        &self,
        dataset_id: Uuid,
        version: &str,
        checksum: &str,
    ) -> Result<Option<Uuid>, StorageError> {
        Storage::record_version_if_changed(self, dataset_id, version, checksum).await
    }

    async fn get_source_state(
        &self,
        source: SourceId,
    ) -> Result<Option<SourceHttpState>, StorageError> {
        Storage::get_source_state(self, source).await
    }

    async fn put_source_state(
        &self,
        source: SourceId,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<(), StorageError> {
        Storage::put_source_state(self, source, etag, last_modified).await
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

/// Object-safe view for `materialize_dataset` (#1.8): finds the
/// latest `dataset_version` and its file children for a given
/// dataset key. Returns `None` when no dataset matches the key,
/// `Some(_)` with an empty `files` vec when the dataset exists but
/// has never been materialised yet.
#[async_trait]
pub trait MaterializeView: Send + Sync + 'static {
    async fn latest_materialise_view(
        &self,
        key: DatasetKey,
    ) -> Result<Option<DatasetLatestFiles>, StorageError>;
}

#[async_trait]
impl MaterializeView for Storage {
    async fn latest_materialise_view(
        &self,
        key: DatasetKey,
    ) -> Result<Option<DatasetLatestFiles>, StorageError> {
        Storage::latest_materialise_view(self, key).await
    }
}

/// What `materialize_dataset` needs from storage in one shot.
#[derive(Debug, Clone)]
pub struct DatasetLatestFiles {
    pub dataset_id: Uuid,
    pub slug: String,
    /// `None` when the dataset has no versions at all (first crawl
    /// hasn't completed yet); `Some` once any version exists. The
    /// tool treats `None` and "version exists but `files` is empty"
    /// identically — both translate to "not materialised yet".
    pub latest_version_id: Option<Uuid>,
    pub files: Vec<DatasetFileRow>,
}

/// Bare-minimum dataset identification for the hot-cache pipeline
/// (#3.6) — id + slug, enough to log what's being promoted/demoted
/// without re-loading the full row.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct CacheCandidate {
    pub id: Uuid,
    pub slug: String,
    pub tier: String,
    /// Number of `query_rows` hits in the lookback window. Always
    /// populated for the candidate-finding query so the worker can
    /// log the actual hit count alongside the slug.
    pub query_hits: i64,
}

/// Cache hit / total ratio for a window — read once per tick by the
/// telemetry exporter. `hits` counts `query_rows` invocations
/// against datasets whose `cached` flag is `true` **at the time
/// the ratio is computed** (when [`CacheState::cache_hit_ratio`]
/// runs) — not at the original call time, since
/// `usage_records` has no per-row cache-state snapshot. `total`
/// is the `query_rows` invocation total over the same window.
/// Both are `i64` to match Postgres `COUNT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, sqlx::FromRow)]
pub struct CacheHitRatio {
    pub hits: i64,
    pub total: i64,
}

impl CacheHitRatio {
    /// Ratio as a fraction in `[0.0, 1.0]`, or `None` when there
    /// were no queries (so the gauge stays at the prior value or
    /// is reported as missing rather than 0/0).
    ///
    /// Cast precision: hits / total are bounded by Postgres COUNT
    /// over the lookback window — even at a million queries/day
    /// they fit in f64's 2^53 mantissa precisely, so the cast is
    /// lossless in practice. The clippy lint is suppressed
    /// locally rather than at the workspace level.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn ratio(&self) -> Option<f64> {
        if self.total == 0 {
            None
        } else {
            Some(self.hits as f64 / self.total as f64)
        }
    }
}

/// Object-safe surface for the #3.6 cache pipeline. Decoupled from
/// the materialisation traits so the etl-worker can hold a
/// minimal Arc<dyn CacheState> instead of the full Storage.
#[async_trait]
pub trait CacheState: Send + Sync + 'static {
    /// Datasets that should be promoted to the parquet cache.
    /// Selection: tier IN ('platinum', 'gold') OR
    /// `query_rows` hits in the last `window_days` ≥ `hit_threshold`.
    /// Already-cached datasets are excluded — promotion is a no-op
    /// when the parquet already exists.
    async fn hot_candidates(
        &self,
        window_days: i32,
        hit_threshold: i64,
    ) -> Result<Vec<CacheCandidate>, StorageError>;

    /// Datasets currently `cached = true` with **zero**
    /// `query_rows` invocations in the last `inactive_days`.
    /// `get_dataset` / `materialize_dataset` calls do *not* keep
    /// a dataset warm — `query_rows` is the signal of "users are
    /// reading rows" that justifies a parquet cache. These are
    /// candidates for demotion — the worker calls
    /// [`Self::demote_dataset`] on each. Platinum / gold tiers
    /// are excluded so editorially-pinned datasets don't churn
    /// out.
    async fn cold_candidates(
        &self,
        inactive_days: i32,
    ) -> Result<Vec<CacheCandidate>, StorageError>;

    /// Mark a dataset as no longer cached. Sets `cached = false`
    /// and clears `cache_path` *iff* the dataset is currently
    /// cached and not editorially pinned. Returns whether the
    /// row was actually changed (`true` = demoted; `false` = the
    /// dataset was promoted to platinum/gold or already
    /// uncached between the candidate scan and the UPDATE, so
    /// the no-op is correct). Doesn't delete the parquet file
    /// itself — the object-store layer's lifecycle policy
    /// handles physical eviction.
    async fn demote_dataset(&self, dataset_id: Uuid) -> Result<bool, StorageError>;

    /// Compute the cache hit ratio over the last `window_days` of
    /// `query_rows` usage. **"Hit"** = `cached = true` **at the
    /// time this method runs** (when the `cache_pipeline` tick
    /// calls into us), *not* at the original `query_rows` call
    /// time — there's no per-row snapshot in `usage_records`.
    /// In practice that's accurate on average because cache
    /// state only changes via the #3.6 pipeline; short bursts
    /// of churn between two 6h ticks could mis-attribute rows.
    /// A perfectly historical ratio needs a `cached_at_call`
    /// column on `usage_records` (v0.2 enhancement). Surfaces
    /// as a Prometheus gauge once #2.10 telemetry lands.
    async fn cache_hit_ratio(&self, window_days: i32) -> Result<CacheHitRatio, StorageError>;
}

#[async_trait]
impl CacheState for Storage {
    async fn hot_candidates(
        &self,
        window_days: i32,
        hit_threshold: i64,
    ) -> Result<Vec<CacheCandidate>, StorageError> {
        Storage::hot_candidates(self, window_days, hit_threshold).await
    }

    async fn cold_candidates(
        &self,
        inactive_days: i32,
    ) -> Result<Vec<CacheCandidate>, StorageError> {
        Storage::cold_candidates(self, inactive_days).await
    }

    async fn demote_dataset(&self, dataset_id: Uuid) -> Result<bool, StorageError> {
        Storage::demote_dataset(self, dataset_id).await
    }

    async fn cache_hit_ratio(&self, window_days: i32) -> Result<CacheHitRatio, StorageError> {
        Storage::cache_hit_ratio(self, window_days).await
    }
}

/// Object-safe writer for `usage_records`. Decoupled from
/// [`DatasetWriter`] so tools that only need to log usage don't drag
/// in the catalog-mutating surface.
#[async_trait]
pub trait UsageRecorder: Send + Sync + 'static {
    async fn record_usage(&self, record: &NewUsageRecord<'_>) -> Result<Uuid, StorageError>;
}

#[async_trait]
impl UsageRecorder for Storage {
    async fn record_usage(&self, record: &NewUsageRecord<'_>) -> Result<Uuid, StorageError> {
        Storage::record_usage(self, record).await
    }
}

/// Audit row to insert into `usage_records`. Borrowed fields keep
/// the call sites zero-allocation when the tool layer already owns
/// the strings.
#[derive(Debug, Clone)]
pub struct NewUsageRecord<'a> {
    pub dataset_id: Uuid,
    pub dataset_version_id: Option<Uuid>,
    /// MCP tool name. CHECK-constrained in SQL.
    pub tool: &'a str,
    /// File format the caller asked for; `None` for read-only tools.
    pub format: Option<&'a str>,
    /// Auth surface (`"anonymous"` / `"user"` / `"api_key"`).
    pub principal_kind: &'a str,
    /// Opaque caller identifier. `None` for anonymous.
    pub principal_id: Option<&'a str>,
    /// Bytes the caller is authorised to fetch. `None` when unknown.
    pub byte_size: Option<i64>,
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

    /// Load the persisted HTTP cache state (`ETag` + `Last-Modified`)
    /// for a source so the ETL driver can send conditional headers
    /// on the next crawl. Returns `None` when this source hasn't
    /// been crawled yet — the driver should fall back to
    /// unconditional fetch in that case.
    pub async fn get_source_state(
        &self,
        source: SourceId,
    ) -> Result<Option<SourceHttpState>, StorageError> {
        sqlx::query_as(
            "SELECT source, etag, last_modified, last_seen_at \
             FROM source_http_state WHERE source = $1",
        )
        .bind(source.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(StorageError::from)
    }

    /// Persist the HTTP cache cues observed on a successful crawl.
    /// `etag` / `last_modified` are nullable because the server may
    /// have emitted only one (or neither). `last_seen_at` is always
    /// refreshed to `now()` — operators read it to answer "when did
    /// the ETL last successfully talk to source X?".
    pub async fn put_source_state(
        &self,
        source: SourceId,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO source_http_state (source, etag, last_modified, last_seen_at) \
             VALUES ($1, $2, $3, now()) \
             ON CONFLICT (source) DO UPDATE SET \
                 etag = EXCLUDED.etag, \
                 last_modified = EXCLUDED.last_modified, \
                 last_seen_at = EXCLUDED.last_seen_at",
        )
        .bind(source.as_str())
        .bind(etag)
        .bind(last_modified)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Insert a new `dataset_versions` row iff `(dataset_id, version)`
    /// hasn't been recorded before. Returns `Some(new_id)` when a row
    /// was inserted, `None` when the version already exists (the
    /// caller already recorded this exact checksum once).
    ///
    /// Used by the ETL driver to record schema-diff history per
    /// `DESIGN.md` §9 (the #1.4 Definition of Done specifies
    /// "schema diff detection stores into `dataset_versions`").
    ///
    /// **Concurrency**: the `INSERT ... ON CONFLICT (dataset_id,
    /// version) DO NOTHING` form makes this idempotent and race-safe.
    /// Two crawlers seeing the same metadata simultaneously will both
    /// derive the same `version` (because the caller folds the
    /// checksum into the label); whichever transaction commits first
    /// inserts, the other sees the conflict and silently no-ops via
    /// `DO NOTHING`. No SELECT, no transaction, no read-skew window.
    ///
    /// **Caller contract**: `version` must be injectively derived
    /// from `checksum` so two distinct checksums never collide on
    /// the same version label (otherwise this method would silently
    /// drop a real change). The ETL driver's `version_string` helper
    /// embeds the full `sha256:<64-hex>` checksum string into the
    /// label so the result is `"<rfc3339-ts>#sha256:<64-hex>"` when
    /// upstream carries a `last_modified_at`, or `"sha256:<64-hex>"`
    /// otherwise. 256 bits of entropy makes this injectivity
    /// mathematical, not probabilistic.
    pub async fn record_version_if_changed(
        &self,
        dataset_id: Uuid,
        version: &str,
        checksum: &str,
    ) -> Result<Option<Uuid>, StorageError> {
        let inserted: Option<(Uuid,)> = sqlx::query_as(
            "INSERT INTO dataset_versions (dataset_id, version, checksum) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (dataset_id, version) DO NOTHING \
             RETURNING id",
        )
        .bind(dataset_id)
        .bind(version)
        .bind(checksum)
        .fetch_optional(&self.pool)
        .await?;
        Ok(inserted.map(|(id,)| id))
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
    pub async fn dataset_cache(&self, key: DatasetKey) -> Result<Option<CacheRef>, StorageError> {
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

    /// Look up the latest version of a dataset together with all of
    /// its `dataset_files` rows. Powers the `materialize_dataset`
    /// MCP tool (#1.8).
    ///
    /// Implementation runs up to three small queries against the
    /// pool (dataset row → latest version → files for that
    /// version). They could be folded into a single CTE but this
    /// path is invoked once per download URL, not in a tight loop,
    /// so the two extra round-trips against a pool-owned connection
    /// don't justify the SQL-side complexity. Re-evaluate if
    /// profiling ever calls this out.
    ///
    /// Returns `None` when no dataset matches; returns `Some(_)`
    /// with `latest_version_id = None` and `files = []` when the
    /// dataset exists but has never been ingested. Returns
    /// `Some(_)` with a populated `latest_version_id` and possibly
    /// empty `files` when versions exist but no concrete files have
    /// been registered (e.g. metadata-only datasets) — the tool
    /// layer translates either to the same "not materialised yet"
    /// error.
    pub async fn latest_materialise_view(
        &self,
        key: DatasetKey,
    ) -> Result<Option<DatasetLatestFiles>, StorageError> {
        let row: Option<(Uuid, String)> = match key {
            DatasetKey::Id(id) => sqlx::query_as("SELECT id, slug FROM datasets WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(StorageError::from)?,
            DatasetKey::Slug(slug) => {
                sqlx::query_as("SELECT id, slug FROM datasets WHERE slug = $1")
                    .bind(slug)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(StorageError::from)?
            }
        };
        let Some((dataset_id, slug)) = row else {
            return Ok(None);
        };

        let latest: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM dataset_versions WHERE dataset_id = $1 \
             ORDER BY fetched_at DESC LIMIT 1",
        )
        .bind(dataset_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StorageError::from)?;

        let latest_version_id = latest.map(|(id,)| id);
        let files: Vec<DatasetFileRow> = if let Some(version_id) = latest_version_id {
            sqlx::query_as(
                "SELECT id, dataset_version_id, format, uri, byte_size, checksum \
                 FROM dataset_files WHERE dataset_version_id = $1 ORDER BY format, id",
            )
            .bind(version_id)
            .fetch_all(&self.pool)
            .await
            .map_err(StorageError::from)?
        } else {
            Vec::new()
        };

        Ok(Some(DatasetLatestFiles {
            dataset_id,
            slug,
            latest_version_id,
            files,
        }))
    }

    /// Insert one `usage_records` row. Returns the new row's UUID.
    ///
    /// Two layers of validation:
    /// 1. **Storage-side** (this method): the `principal_kind` ↔
    ///    `principal_id` invariant is checked before the SQL round-
    ///    trip. NULL `principal_id` is only valid for `anonymous`;
    ///    non-anonymous kinds require a non-empty id. Violations
    ///    surface as [`StorageError::InvalidArgument`] (mappable to
    ///    a 4xx) instead of the opaque CHECK-constraint error.
    /// 2. **SQL-side**: CHECK constraints on `tool`, `format`,
    ///    `principal_kind`, and the same `principal_id` consistency
    ///    rule live in `migrations/0006_usage_records.sql`. They
    ///    keep future callers honest even if they bypass this
    ///    method.
    pub async fn record_usage(&self, record: &NewUsageRecord<'_>) -> Result<Uuid, StorageError> {
        validate_principal_pair(record.principal_kind, record.principal_id)?;
        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO usage_records \
                 (dataset_id, dataset_version_id, tool, format, \
                  principal_kind, principal_id, byte_size) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             RETURNING id",
        )
        .bind(record.dataset_id)
        .bind(record.dataset_version_id)
        .bind(record.tool)
        .bind(record.format)
        .bind(record.principal_kind)
        .bind(record.principal_id)
        .bind(record.byte_size)
        .fetch_one(&self.pool)
        .await
        .map_err(StorageError::from)?;
        Ok(row.0)
    }

    /// #3.6 hot-cache pipeline: candidates for promotion.
    ///
    /// Selection rule: tier IN ('platinum', 'gold') OR the dataset
    /// received ≥ `hit_threshold` `query_rows` calls in the last
    /// `window_days`. Already-cached datasets are excluded — re-
    /// materialisation is a separate operation.
    ///
    /// Returned in deterministic order (tier rank asc — platinum
    /// first, then gold, silver, bronze; then hit count desc;
    /// then slug asc) so a worker can checkpoint progress within
    /// the candidate list.
    pub async fn hot_candidates(
        &self,
        window_days: i32,
        hit_threshold: i64,
    ) -> Result<Vec<CacheCandidate>, StorageError> {
        // Effective tier = COALESCE(tier_override, tier). The
        // schema (migrations/0001_init.sql) makes tier_override
        // win when an admin pins a dataset, so the candidate
        // query has to look at that column or it'll miss
        // editorial pins.
        let rows: Vec<CacheCandidate> = sqlx::query_as(
            "SELECT d.id, d.slug, \
                    COALESCE(d.tier_override, d.tier) AS tier, \
                    COALESCE(u.hits, 0)::bigint AS query_hits \
             FROM datasets d \
             LEFT JOIN ( \
                SELECT dataset_id, COUNT(*)::bigint AS hits \
                FROM usage_records \
                WHERE tool = 'query_rows' \
                  AND requested_at >= now() - ($1::int * INTERVAL '1 day') \
                GROUP BY dataset_id \
             ) u ON u.dataset_id = d.id \
             WHERE NOT d.cached \
               AND ( \
                 COALESCE(d.tier_override, d.tier) IN ('platinum', 'gold') \
                 OR COALESCE(u.hits, 0) >= $2 \
               ) \
             ORDER BY \
               CASE COALESCE(d.tier_override, d.tier) \
                 WHEN 'platinum' THEN 1 \
                 WHEN 'gold' THEN 2 \
                 WHEN 'silver' THEN 3 \
                 ELSE 4 \
               END, \
               COALESCE(u.hits, 0) DESC, \
               d.slug ASC",
        )
        .bind(window_days)
        .bind(hit_threshold)
        .fetch_all(&self.pool)
        .await
        .map_err(StorageError::from)?;
        Ok(rows)
    }

    /// #3.6 hot-cache pipeline: candidates for demotion.
    ///
    /// Selection rule: currently `cached = true`, tier NOT IN
    /// ('platinum', 'gold') (editorial pins stay), AND no
    /// `query_rows` hit in the last `inactive_days`.
    pub async fn cold_candidates(
        &self,
        inactive_days: i32,
    ) -> Result<Vec<CacheCandidate>, StorageError> {
        // Same effective-tier rule as `hot_candidates` —
        // tier_override wins.
        let rows: Vec<CacheCandidate> = sqlx::query_as(
            "SELECT d.id, d.slug, \
                    COALESCE(d.tier_override, d.tier) AS tier, \
                    COALESCE(u.hits, 0)::bigint AS query_hits \
             FROM datasets d \
             LEFT JOIN ( \
                SELECT dataset_id, COUNT(*)::bigint AS hits \
                FROM usage_records \
                WHERE tool = 'query_rows' \
                  AND requested_at >= now() - ($1::int * INTERVAL '1 day') \
                GROUP BY dataset_id \
             ) u ON u.dataset_id = d.id \
             WHERE d.cached \
               AND COALESCE(d.tier_override, d.tier) NOT IN ('platinum', 'gold') \
               AND COALESCE(u.hits, 0) = 0 \
             ORDER BY d.slug ASC",
        )
        .bind(inactive_days)
        .fetch_all(&self.pool)
        .await
        .map_err(StorageError::from)?;
        Ok(rows)
    }

    /// #3.6 hot-cache pipeline: clear a dataset's cached state.
    ///
    /// Sets `cached = false` and `cache_path = NULL`. The actual
    /// parquet file in `SeaweedFS` is left for the object-store
    /// lifecycle policy to garbage-collect; tracking that here
    /// would be a layering violation.
    ///
    /// The UPDATE carries the same eligibility predicate as
    /// `cold_candidates` (`cached = true` AND effective tier
    /// not pinned to platinum/gold). This closes the race where
    /// an admin promotes a dataset to platinum/gold *after*
    /// `cold_candidates` runs but *before* the demote fires —
    /// the predicate makes the UPDATE a no-op and the returned
    /// `false` lets the caller distinguish "really demoted"
    /// from "race lost". A missing `id` also returns `false`
    /// rather than silently succeeding.
    pub async fn demote_dataset(&self, dataset_id: Uuid) -> Result<bool, StorageError> {
        let result = sqlx::query(
            "UPDATE datasets \
             SET cached = false, cache_path = NULL \
             WHERE id = $1 \
               AND cached \
               AND COALESCE(tier_override, tier) NOT IN ('platinum', 'gold')",
        )
        .bind(dataset_id)
        .execute(&self.pool)
        .await
        .map_err(StorageError::from)?;
        Ok(result.rows_affected() > 0)
    }

    /// #3.6 hot-cache pipeline: aggregate cache hit ratio for a
    /// `query_rows` window. A "hit" is a `query_rows` invocation
    /// whose dataset has `cached = true` **right now** — that is,
    /// when this aggregation method runs, not at the original
    /// `query_rows` call time. (`usage_records` has no per-row
    /// snapshot of the cache flag, so this is the closest
    /// approximation; see paragraph below for the drift bound.)
    ///
    /// The join is against the *current* `datasets.cached` flag
    /// because `usage_records` has no per-row snapshot of the
    /// cache state at call time. In practice a dataset's cache
    /// state only changes via this pipeline (which runs every
    /// 6 hours), so the ratio is accurate on average — but a
    /// short burst of churn between two ticks could mis-attribute
    /// individual rows. A perfectly historical hit ratio would
    /// need a separate `cached_at_call` column on
    /// `usage_records`; v0.2 enhancement if telemetry drift
    /// becomes a concern.
    pub async fn cache_hit_ratio(&self, window_days: i32) -> Result<CacheHitRatio, StorageError> {
        let row: CacheHitRatio = sqlx::query_as(
            "SELECT \
                COALESCE(SUM(CASE WHEN d.cached THEN 1 ELSE 0 END), 0)::bigint AS hits, \
                COUNT(*)::bigint AS total \
             FROM usage_records u \
             JOIN datasets d ON d.id = u.dataset_id \
             WHERE u.tool = 'query_rows' \
               AND u.requested_at >= now() - ($1::int * INTERVAL '1 day')",
        )
        .bind(window_days)
        .fetch_one(&self.pool)
        .await
        .map_err(StorageError::from)?;
        Ok(row)
    }
}

/// Enforce the `principal_kind` ↔ `principal_id` invariant the
/// migration documents and the SQL CHECK constraint protects.
/// Kept private to the storage layer because the rule is intrinsic
/// to the `usage_records` shape — callers express it implicitly by
/// supplying or omitting `principal_id` and the validator either
/// accepts or returns an `InvalidArgument`.
fn validate_principal_pair(kind: &str, id: Option<&str>) -> Result<(), StorageError> {
    match (kind, id) {
        ("anonymous", Some(_)) => Err(StorageError::InvalidArgument(
            "principal_id must be NULL when principal_kind = anonymous".into(),
        )),
        ("anonymous", None) => Ok(()),
        (_, None) => Err(StorageError::InvalidArgument(format!(
            "principal_id is required when principal_kind = {kind}"
        ))),
        (_, Some("")) => Err(StorageError::InvalidArgument(format!(
            "principal_id must not be empty when principal_kind = {kind}"
        ))),
        (_, Some(_)) => Ok(()),
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
    /// Caller-supplied data violates a cross-field invariant the
    /// storage layer enforces before the SQL round-trip. Distinct
    /// from [`Self::Database`] (which surfaces backend errors) so
    /// callers can map invariant failures to a 4xx instead of a 5xx.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
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

    /// `source_http_state` upsert semantics (#1.4d.2): first put
    /// inserts a row; second put on the same `source` updates the
    /// cues. `last_seen_at` always advances. Operators read this
    /// table to answer "when did the ETL last talk to source X?".
    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn source_http_state_upserts_on_repeat_put() {
        let (storage, _container) = fresh_storage().await;

        // Initial put → row exists with the cues we set.
        storage
            .put_source_state(
                SourceId::DataGovTw,
                Some("\"v1-etag\""),
                Some("Wed, 14 Apr 2026 03:30:00 GMT"),
            )
            .await
            .expect("first put");
        let first = storage
            .get_source_state(SourceId::DataGovTw)
            .await
            .expect("get ok")
            .expect("row present");
        assert_eq!(first.etag.as_deref(), Some("\"v1-etag\""));
        assert_eq!(
            first.last_modified.as_deref(),
            Some("Wed, 14 Apr 2026 03:30:00 GMT"),
        );

        // Second put with different cues → updates in place; row
        // count stays at 1 (the PRIMARY KEY on `source` enforces it).
        storage
            .put_source_state(
                SourceId::DataGovTw,
                Some("\"v2-etag\""),
                Some("Thu, 15 Apr 2026 03:30:00 GMT"),
            )
            .await
            .expect("second put");
        let second = storage
            .get_source_state(SourceId::DataGovTw)
            .await
            .expect("get ok")
            .expect("row present");
        assert_eq!(second.etag.as_deref(), Some("\"v2-etag\""));
        assert!(
            second.last_seen_at >= first.last_seen_at,
            "last_seen_at advances or stays the same on update",
        );

        let count: (i64,) = sqlx::query_as("SELECT count(*) FROM source_http_state")
            .fetch_one(storage.pool())
            .await
            .expect("count");
        assert_eq!(count.0, 1, "exactly one row per source (PK enforces it)");

        // Missing source → None, not an error.
        let twse = storage
            .get_source_state(SourceId::Twse)
            .await
            .expect("get ok");
        assert!(twse.is_none());
    }

    /// Pin the natural-key dedup semantic: returning to a
    /// previously-seen `(dataset_id, version)` pair must no-op via
    /// `ON CONFLICT DO NOTHING`. A metadata oscillation A → B → A
    /// records exactly two rows, not three.
    ///
    /// This is the deliberate trade-off the storage layer makes:
    /// concurrency safety (no read-then-write race) at the cost of
    /// losing oscillation history. Pin it here so a future refactor
    /// back to "differs from latest" without proper locking fails CI.
    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn record_version_oscillation_collapses_to_seen_set() {
        let (storage, _container) = fresh_storage().await;
        let domain_id = realestate_land_id(&storage).await;
        let dataset_id = storage
            .upsert_dataset(domain_id, SourceId::DataGovTw, &sample_metadata())
            .await
            .expect("seed");

        let v_a = "ts#sha256:aaaa";
        let v_b = "ts#sha256:bbbb";
        let a1 = storage
            .record_version_if_changed(dataset_id, v_a, "sha256:aaaa")
            .await
            .expect("A first");
        let b1 = storage
            .record_version_if_changed(dataset_id, v_b, "sha256:bbbb")
            .await
            .expect("B");
        let a2 = storage
            .record_version_if_changed(dataset_id, v_a, "sha256:aaaa")
            .await
            .expect("A second");

        assert!(a1.is_some(), "A first records");
        assert!(b1.is_some(), "B records");
        assert!(
            a2.is_none(),
            "A again no-ops via natural-key dedup (not a new row)",
        );

        let count: (i64,) =
            sqlx::query_as("SELECT count(*) FROM dataset_versions WHERE dataset_id = $1")
                .bind(dataset_id)
                .fetch_one(storage.pool())
                .await
                .expect("count");
        assert_eq!(count.0, 2, "A → B → A collapses to two rows");
    }

    /// Schema-diff contract for #1.4d: the storage layer dedups on
    /// `(dataset_id, version)` (the table's UNIQUE constraint backed
    /// by `ON CONFLICT DO NOTHING`). First call with a new version
    /// inserts; repeat with the same `(dataset_id, version)` is a
    /// no-op; a different version inserts another row. The
    /// `checksum` argument is stored but does NOT participate in
    /// the dedup decision.
    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn record_version_dedupes_on_dataset_id_and_version() {
        let (storage, _container) = fresh_storage().await;
        let domain_id = realestate_land_id(&storage).await;
        let dataset_id = storage
            .upsert_dataset(domain_id, SourceId::DataGovTw, &sample_metadata())
            .await
            .expect("seed");

        // First call records a fresh version.
        let v1 = storage
            .record_version_if_changed(dataset_id, "2026-04-15", "checksum-A")
            .await
            .expect("first version");
        assert!(v1.is_some(), "first call must insert");

        // Same checksum → no new row.
        let v2 = storage
            .record_version_if_changed(dataset_id, "2026-04-15", "checksum-A")
            .await
            .expect("no-op");
        assert!(v2.is_none(), "matching checksum must not insert");

        // Different checksum → new row.
        let v3 = storage
            .record_version_if_changed(dataset_id, "2026-05-01", "checksum-B")
            .await
            .expect("second version");
        assert!(v3.is_some(), "changed checksum must insert");
        assert_ne!(v1.unwrap(), v3.unwrap());

        let count: (i64,) =
            sqlx::query_as("SELECT count(*) FROM dataset_versions WHERE dataset_id = $1")
                .bind(dataset_id)
                .fetch_one(storage.pool())
                .await
                .expect("count");
        assert_eq!(count.0, 2, "exactly two version rows after A → A → B");
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

    /// `materialize_dataset` (#1.8) needs the dataset id + latest
    /// version + every `dataset_files` row in one round-trip.
    /// Verifies the three observable states: dataset missing →
    /// `None`; dataset present but no version → `Some(_)` with
    /// `latest_version_id = None / files = []`; dataset with
    /// multiple versions → returns ONLY the newest version's files
    /// (the tool layer never wants to mix versions).
    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn latest_materialise_view_returns_only_newest_version_files() {
        let (storage, _container) = fresh_storage().await;
        let domain_id = realestate_land_id(&storage).await;

        // Missing dataset → None.
        let none = storage
            .latest_materialise_view(DatasetKey::Slug("never-existed".to_owned()))
            .await
            .expect("query ok");
        assert!(none.is_none(), "missing slug yields None");

        // Insert dataset + two versions, oldest first.
        let dataset_id = storage
            .upsert_dataset(domain_id, SourceId::DataGovTw, &sample_metadata())
            .await
            .expect("upsert");

        let v_old = storage
            .record_version_if_changed(dataset_id, "2026-04-01", "sha256:old")
            .await
            .expect("v_old")
            .expect("inserted");
        let v_new = storage
            .record_version_if_changed(dataset_id, "2026-05-01", "sha256:new")
            .await
            .expect("v_new")
            .expect("inserted");

        // Seed dataset_files manually — the ETL doesn't write them
        // yet (that's a separate sub-issue), so the tool relies on
        // whoever populates the table.
        sqlx::query(
            "INSERT INTO dataset_files (dataset_version_id, format, uri, byte_size, checksum) \
             VALUES ($1, 'parquet', 'file:///cache/old.parquet', 100, 'cs-old'), \
                    ($2, 'parquet', 'file:///cache/new.parquet', 200, 'cs-new'), \
                    ($2, 'csv',     'file:///cache/new.csv',     300, NULL)",
        )
        .bind(v_old)
        .bind(v_new)
        .execute(storage.pool())
        .await
        .expect("seed files");

        let view = storage
            .latest_materialise_view(DatasetKey::Id(dataset_id))
            .await
            .expect("query ok")
            .expect("dataset present");
        assert_eq!(view.dataset_id, dataset_id);
        assert_eq!(view.slug, "real-estate-prices");
        assert_eq!(view.latest_version_id, Some(v_new));
        assert_eq!(view.files.len(), 2, "only newest version's files");
        let formats: std::collections::HashSet<_> =
            view.files.iter().map(|f| f.format.as_str()).collect();
        assert!(formats.contains("parquet"));
        assert!(formats.contains("csv"));
        assert!(
            !view
                .files
                .iter()
                .any(|f| f.uri == "file:///cache/old.parquet"),
            "older version's file must not leak into latest view"
        );
    }

    /// Pin the `usage_records` write contract: a successful insert
    /// returns a fresh UUID; the row carries the CHECK-constrained
    /// enum strings; concurrent inserts produce distinct ids.
    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn record_usage_persists_row_with_uuid() {
        let (storage, _container) = fresh_storage().await;
        let domain_id = realestate_land_id(&storage).await;
        let dataset_id = storage
            .upsert_dataset(domain_id, SourceId::DataGovTw, &sample_metadata())
            .await
            .expect("upsert");
        let version_id = storage
            .record_version_if_changed(dataset_id, "2026-05-01", "sha256:1")
            .await
            .expect("version")
            .expect("inserted");

        let uid = storage
            .record_usage(&NewUsageRecord {
                dataset_id,
                dataset_version_id: Some(version_id),
                tool: "materialize_dataset",
                format: Some("parquet"),
                principal_kind: "anonymous",
                principal_id: None,
                byte_size: Some(2048),
            })
            .await
            .expect("insert");
        assert_ne!(uid, Uuid::nil());

        // Round-trip read: confirm the row landed with the right
        // shape and the FK to dataset_versions is wired.
        let row: (
            Uuid,
            Uuid,
            Option<Uuid>,
            String,
            Option<String>,
            String,
            Option<i64>,
        ) = sqlx::query_as(
            "SELECT id, dataset_id, dataset_version_id, tool, format, principal_kind, byte_size \
                 FROM usage_records WHERE id = $1",
        )
        .bind(uid)
        .fetch_one(storage.pool())
        .await
        .expect("readback");
        assert_eq!(row.0, uid);
        assert_eq!(row.1, dataset_id);
        assert_eq!(row.2, Some(version_id));
        assert_eq!(row.3, "materialize_dataset");
        assert_eq!(row.4.as_deref(), Some("parquet"));
        assert_eq!(row.5, "anonymous");
        assert_eq!(row.6, Some(2048));

        // CHECK constraint trips on bad enum value.
        let bad = storage
            .record_usage(&NewUsageRecord {
                dataset_id,
                dataset_version_id: None,
                tool: "made_up_tool",
                format: None,
                principal_kind: "anonymous",
                principal_id: None,
                byte_size: None,
            })
            .await;
        assert!(bad.is_err(), "CHECK constraint must reject unknown tool");
    }

    /// Pin the storage-side `principal_kind` ↔ `principal_id`
    /// validation. Each forbidden combination must surface as
    /// `InvalidArgument` (not as a SQL-level CHECK violation) so
    /// callers can map to 4xx without inspecting Postgres error
    /// codes. The CHECK constraint in `0006_usage_records.sql` is
    /// belt-and-braces — see `record_usage_db_check_blocks_ambiguous_pair`.
    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn record_usage_validates_principal_pair_invariant() {
        let (storage, _container) = fresh_storage().await;
        let domain_id = realestate_land_id(&storage).await;
        let dataset_id = storage
            .upsert_dataset(domain_id, SourceId::DataGovTw, &sample_metadata())
            .await
            .expect("upsert");

        // anonymous + non-null id → InvalidArgument
        let err = storage
            .record_usage(&NewUsageRecord {
                dataset_id,
                dataset_version_id: None,
                tool: "materialize_dataset",
                format: Some("parquet"),
                principal_kind: "anonymous",
                principal_id: Some("smuggled"),
                byte_size: None,
            })
            .await
            .expect_err("must reject anonymous+id");
        assert!(matches!(err, StorageError::InvalidArgument(_)));

        // user + None → InvalidArgument
        let err = storage
            .record_usage(&NewUsageRecord {
                dataset_id,
                dataset_version_id: None,
                tool: "materialize_dataset",
                format: Some("parquet"),
                principal_kind: "user",
                principal_id: None,
                byte_size: None,
            })
            .await
            .expect_err("must reject user without id");
        assert!(matches!(err, StorageError::InvalidArgument(_)));

        // api_key + "" → InvalidArgument
        let err = storage
            .record_usage(&NewUsageRecord {
                dataset_id,
                dataset_version_id: None,
                tool: "materialize_dataset",
                format: Some("parquet"),
                principal_kind: "api_key",
                principal_id: Some(""),
                byte_size: None,
            })
            .await
            .expect_err("must reject api_key with empty id");
        assert!(matches!(err, StorageError::InvalidArgument(_)));

        // Happy path: user + non-empty id → insert succeeds.
        let ok = storage
            .record_usage(&NewUsageRecord {
                dataset_id,
                dataset_version_id: None,
                tool: "materialize_dataset",
                format: Some("parquet"),
                principal_kind: "user",
                principal_id: Some("550e8400-e29b-41d4-a716-446655440000"),
                byte_size: None,
            })
            .await
            .expect("happy path insert");
        assert_ne!(ok, Uuid::nil());
    }

    /// Belt: bypass the storage-side validation and confirm the SQL
    /// CHECK constraint also blocks the ambiguous combination. Uses
    /// a raw `sqlx::query` so we exercise the DB-level enforcement
    /// directly, not the Rust-side validator.
    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn record_usage_db_check_blocks_ambiguous_pair() {
        let (storage, _container) = fresh_storage().await;
        let domain_id = realestate_land_id(&storage).await;
        let dataset_id = storage
            .upsert_dataset(domain_id, SourceId::DataGovTw, &sample_metadata())
            .await
            .expect("upsert");

        let raw_insert: Result<(Uuid,), _> = sqlx::query_as(
            "INSERT INTO usage_records \
                 (dataset_id, dataset_version_id, tool, format, \
                  principal_kind, principal_id, byte_size) \
             VALUES ($1, NULL, 'materialize_dataset', NULL, 'anonymous', 'should-be-null', NULL) \
             RETURNING id",
        )
        .bind(dataset_id)
        .fetch_one(storage.pool())
        .await;
        assert!(
            raw_insert.is_err(),
            "SQL CHECK must reject anonymous + non-null principal_id"
        );
    }

    // ════════════════════════════════════════════════════════════
    //  #3.6 hot-cache pipeline — testcontainers coverage
    // ════════════════════════════════════════════════════════════
    //
    // The cache_pipeline storage methods (hot_candidates,
    // cold_candidates, demote_dataset, cache_hit_ratio) ship four
    // non-trivial SQL queries with eligibility predicates,
    // COALESCE(tier_override, tier) joins, partial-index-friendly
    // shapes, and an UPDATE race-guard. The unit tests in the
    // worker crate use mocks; these `#[ignore]` testcontainers
    // tests catch SQL drift before it lands in production.

    /// Force a dataset into `cached = true` (the upsert path
    /// preserves `cached` / `cache_path` so we can't drive it
    /// through the catalog API). Used by the cache-pipeline tests
    /// below.
    async fn force_cache_state(
        storage: &Storage,
        dataset_id: Uuid,
        cached: bool,
        tier: Option<&str>,
        tier_override: Option<&str>,
    ) {
        sqlx::query(
            "UPDATE datasets SET cached = $2, \
                cache_path = CASE WHEN $2 THEN 'file:///tmp/' || id::text || '.parquet' ELSE NULL END, \
                tier = COALESCE($3, tier), \
                tier_override = $4 \
             WHERE id = $1",
        )
        .bind(dataset_id)
        .bind(cached)
        .bind(tier)
        .bind(tier_override)
        .execute(storage.pool())
        .await
        .expect("force cache state");
    }

    async fn insert_query_rows_usage(storage: &Storage, dataset_id: Uuid, count: usize) {
        for _ in 0..count {
            storage
                .record_usage(&NewUsageRecord {
                    dataset_id,
                    dataset_version_id: None,
                    tool: "query_rows",
                    format: None,
                    principal_kind: "anonymous",
                    principal_id: None,
                    byte_size: None,
                })
                .await
                .expect("record query_rows usage");
        }
    }

    /// Locks: `hot_candidates` returns platinum/gold via the tier
    /// rule even when zero hits, AND silver+ via the hit-threshold
    /// rule. Already-cached datasets are excluded. Order is
    /// platinum → gold → (hot silver) by tier-rank-asc.
    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn hot_candidates_selects_platinum_gold_or_above_threshold() {
        let (storage, _container) = fresh_storage().await;
        let domain_id = realestate_land_id(&storage).await;

        let mut ids = Vec::new();
        for slug in ["plat-1", "gold-1", "silver-popular", "silver-quiet"] {
            let m = DatasetMetadata {
                source_id: slug.into(),
                slug: slug.into(),
                title_i18n: BTreeMap::from([("zh-TW".into(), slug.to_owned())]),
                description_i18n: BTreeMap::new(),
                license: "CC0".into(),
                publisher: None,
                update_frequency: None,
                original_url: None,
                last_modified_at: None,
                upstream_categories: vec![],
            };
            let id = storage
                .upsert_dataset(domain_id, SourceId::DataGovTw, &m)
                .await
                .expect("upsert");
            ids.push(id);
        }
        // Force tiers (upsert preserves them on first insert as
        // 'bronze', so we have to set them explicitly).
        force_cache_state(&storage, ids[0], false, Some("platinum"), None).await;
        force_cache_state(&storage, ids[1], false, Some("gold"), None).await;
        force_cache_state(&storage, ids[2], false, Some("silver"), None).await;
        force_cache_state(&storage, ids[3], false, Some("silver"), None).await;
        // silver-popular crosses the threshold; silver-quiet stays below.
        insert_query_rows_usage(&storage, ids[2], 60).await;
        insert_query_rows_usage(&storage, ids[3], 5).await;

        let hot = storage.hot_candidates(7, 50).await.expect("hot_candidates");
        let slugs: Vec<&str> = hot.iter().map(|c| c.slug.as_str()).collect();
        assert!(
            slugs.contains(&"plat-1")
                && slugs.contains(&"gold-1")
                && slugs.contains(&"silver-popular"),
            "expected plat-1 + gold-1 + silver-popular in hot list, got {slugs:?}",
        );
        assert!(
            !slugs.contains(&"silver-quiet"),
            "silver-quiet under threshold should be excluded",
        );
        // Order: platinum first, then gold, then silver-popular.
        assert_eq!(slugs[0], "plat-1", "tier rank: platinum=1 first");
        assert_eq!(slugs[1], "gold-1", "tier rank: gold=2 second");
    }

    /// `hot_candidates` respects `tier_override`: a base 'bronze'
    /// dataset with `tier_override = 'platinum'` must show up
    /// even with zero hits.
    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn hot_candidates_respects_tier_override() {
        let (storage, _container) = fresh_storage().await;
        let domain_id = realestate_land_id(&storage).await;
        let m = DatasetMetadata {
            source_id: "pinned".into(),
            slug: "pinned".into(),
            title_i18n: BTreeMap::from([("zh-TW".into(), "pinned".to_owned())]),
            description_i18n: BTreeMap::new(),
            license: "CC0".into(),
            publisher: None,
            update_frequency: None,
            original_url: None,
            last_modified_at: None,
            upstream_categories: vec![],
        };
        let id = storage
            .upsert_dataset(domain_id, SourceId::DataGovTw, &m)
            .await
            .expect("upsert");
        force_cache_state(&storage, id, false, Some("bronze"), Some("platinum")).await;

        let hot = storage.hot_candidates(7, 50).await.expect("hot_candidates");
        assert!(hot.iter().any(|c| c.slug == "pinned"));
        // CacheCandidate.tier returns the *effective* tier.
        let entry = hot.iter().find(|c| c.slug == "pinned").unwrap();
        assert_eq!(
            entry.tier, "platinum",
            "effective tier should be the override"
        );
    }

    /// `cold_candidates`: returns silver/bronze that are cached but
    /// have zero `query_rows` hits in the window. Excludes platinum/
    /// gold via the effective-tier rule.
    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn cold_candidates_selects_silent_unpinned_cached_datasets() {
        let (storage, _container) = fresh_storage().await;
        let domain_id = realestate_land_id(&storage).await;
        let mut ids = Vec::new();
        for slug in ["bronze-stale", "silver-active", "gold-pinned"] {
            let m = DatasetMetadata {
                source_id: slug.into(),
                slug: slug.into(),
                title_i18n: BTreeMap::from([("zh-TW".into(), slug.to_owned())]),
                description_i18n: BTreeMap::new(),
                license: "CC0".into(),
                publisher: None,
                update_frequency: None,
                original_url: None,
                last_modified_at: None,
                upstream_categories: vec![],
            };
            let id = storage
                .upsert_dataset(domain_id, SourceId::DataGovTw, &m)
                .await
                .expect("upsert");
            ids.push(id);
        }
        force_cache_state(&storage, ids[0], true, Some("bronze"), None).await;
        force_cache_state(&storage, ids[1], true, Some("silver"), None).await;
        force_cache_state(&storage, ids[2], true, Some("gold"), None).await;
        // silver-active has a recent hit; bronze-stale and gold-pinned have none.
        insert_query_rows_usage(&storage, ids[1], 3).await;

        let cold = storage.cold_candidates(30).await.expect("cold");
        let slugs: Vec<&str> = cold.iter().map(|c| c.slug.as_str()).collect();
        assert!(
            slugs.contains(&"bronze-stale"),
            "bronze with no hits → cold"
        );
        assert!(
            !slugs.contains(&"silver-active"),
            "silver with hits stays warm"
        );
        assert!(
            !slugs.contains(&"gold-pinned"),
            "gold editorial pin protected"
        );
    }

    /// `demote_dataset` returns true on successful demote, false on
    /// race (e.g. the dataset became gold between `cold_candidates`
    /// and the UPDATE).
    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn demote_dataset_returns_false_on_race_with_promotion() {
        let (storage, _container) = fresh_storage().await;
        let domain_id = realestate_land_id(&storage).await;
        let m = DatasetMetadata {
            source_id: "racing".into(),
            slug: "racing".into(),
            title_i18n: BTreeMap::from([("zh-TW".into(), "racing".to_owned())]),
            description_i18n: BTreeMap::new(),
            license: "CC0".into(),
            publisher: None,
            update_frequency: None,
            original_url: None,
            last_modified_at: None,
            upstream_categories: vec![],
        };
        let id = storage
            .upsert_dataset(domain_id, SourceId::DataGovTw, &m)
            .await
            .expect("upsert");

        // Eligible (cached, bronze, no pin) → demote succeeds.
        force_cache_state(&storage, id, true, Some("bronze"), None).await;
        let demoted = storage.demote_dataset(id).await.expect("demote");
        assert!(demoted, "eligible dataset demotes");

        // Re-promote + pin via tier_override → demote no-ops.
        force_cache_state(&storage, id, true, Some("bronze"), Some("platinum")).await;
        let demoted_again = storage.demote_dataset(id).await.expect("demote pinned");
        assert!(!demoted_again, "pinned dataset must not demote");
    }

    /// `cache_hit_ratio`: counts `query_rows` usage in window; "hit"
    /// is current cached=true at aggregation time.
    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p storage -- --ignored`"]
    async fn cache_hit_ratio_counts_query_rows_against_current_cached_flag() {
        let (storage, _container) = fresh_storage().await;
        let domain_id = realestate_land_id(&storage).await;
        let mut ids = Vec::new();
        for slug in ["hot", "cold"] {
            let m = DatasetMetadata {
                source_id: slug.into(),
                slug: slug.into(),
                title_i18n: BTreeMap::from([("zh-TW".into(), slug.to_owned())]),
                description_i18n: BTreeMap::new(),
                license: "CC0".into(),
                publisher: None,
                update_frequency: None,
                original_url: None,
                last_modified_at: None,
                upstream_categories: vec![],
            };
            let id = storage
                .upsert_dataset(domain_id, SourceId::DataGovTw, &m)
                .await
                .expect("upsert");
            ids.push(id);
        }
        force_cache_state(&storage, ids[0], true, Some("silver"), None).await;
        force_cache_state(&storage, ids[1], false, Some("silver"), None).await;
        insert_query_rows_usage(&storage, ids[0], 3).await; // 3 hits
        insert_query_rows_usage(&storage, ids[1], 2).await; // 2 misses

        let ratio = storage.cache_hit_ratio(7).await.expect("ratio");
        assert_eq!(ratio.total, 5, "5 query_rows rows total");
        assert_eq!(ratio.hits, 3, "3 against cached=true");
        let v = ratio.ratio().expect("ratio Some");
        assert!((v - 0.6).abs() < 1e-9);
    }
}
