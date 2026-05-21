//! `get_sample` MCP tool — returns up to N rows from a cached dataset
//! under one of three strategies (head / random / stratified).
//!
//! Per #3.3 Definition of Done:
//!  - `strategy`: `"head" | "random" | "stratified"`; default `"head"`.
//!  - `n`: number of rows requested; default 10, capped at 1000.
//!  - `stratified` requires `stratify_col`; head / random reject it.
//!  - random uses Polars' reservoir sampling (single pass, bounded
//!    memory), via the workspace `random` polars feature.
//!
//! The scan is bounded by [`MAX_SAMPLE_SCAN_ROWS`] so even a 100M-row
//! dataset doesn't have to materialise fully — the sample is drawn
//! from the first N scan-time rows, and the response carries
//! `sampled: true` (matching `describe_schema`) when the underlying
//! file is larger than the cap.
//!
//! ## Strategy notes
//!
//! - **head**: deterministic; the first `n` rows in storage order.
//! - **random**: Polars `sample_n` with `with_replacement=false`,
//!   reservoir-style. Accepts an optional `seed` for reproducibility
//!   (tests pass a fixed seed so the result is deterministic).
//! - **stratified**: partition by `stratify_col`, take `ceil(n / k)`
//!   per group (`k` = distinct strata count, also capped at `n`),
//!   then trim the total to `n`. Per-group samples use reservoir
//!   too. Returns fewer than `n` rows only when the dataset
//!   itself has fewer rows than `n` *or* a group is smaller than its
//!   target share; we don't oversample to fill the quota.
//!
//! ## Error wire shape
//!
//! Mirrors `describe_schema`: tool-level invariant violations route
//! through a local [`SampleError`] enum so the
//! `EngineError::Polars` op-label contract from #3.1 stays clean.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mcp_core::{
    DatasetEngine, DatasetSource, EngineError, LoadOptions, ToolDescriptor, ToolError, ToolHandler,
};
use polars::prelude::*;
use serde_json::{Map, Value, json};
use storage::{CacheRef, DatasetCacheLookup, DatasetKey};
use thiserror::Error;
use uuid::Uuid;

/// MCP tool name. Stable identifier — clients pin to this string.
pub const TOOL_NAME: &str = "get_sample";

/// Default `n` per the issue's Definition of Done.
pub const DEFAULT_N: u32 = 10;

/// Hard cap on `n` per the issue's Definition of Done. Bounds the
/// JSON-rendering cost (no agent can usefully eyeball more rows than
/// this in a single turn).
pub const MAX_N: u32 = 1000;

/// Row cap applied to the scan that feeds the sampling step. Matches
/// `describe_schema::MAX_SAMPLE_ROWS` so the two rich tools have
/// matching memory bounds; `sampled: true` flags the response when
/// the underlying file was larger.
pub const MAX_SAMPLE_SCAN_ROWS: u32 = 100_000;

/// Per-call deadline. Larger than `query_rows`'s 5s because a 100k-row
/// scan + `partition_by` for stratified sampling does more work than
/// `query_rows`'s single `SELECT`.
const SAMPLE_TIMEOUT: Duration = Duration::from_secs(10);

/// Accepted `strategy` wire names. Adding a new variant requires
/// extending [`SampleStrategy`] + [`SampleStrategy::from_wire`] + the
/// `match` in `draw_sample` (compiler-enforced exhaustiveness).
const ACCEPTED_STRATEGIES: &[&str] = &["head", "random", "stratified"];

#[derive(Debug, Clone)]
enum SampleStrategy {
    Head,
    Random { seed: Option<u64> },
    Stratified { stratify_col: String },
}

impl SampleStrategy {
    fn as_wire(&self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::Random { .. } => "random",
            Self::Stratified { .. } => "stratified",
        }
    }
}

/// Reads from any [`DatasetCacheLookup`]; production wires a
/// `storage::Storage`, tests plug in an in-memory stub.
#[derive(Clone)]
pub struct GetSampleTool {
    lookup: Arc<dyn DatasetCacheLookup>,
}

impl GetSampleTool {
    pub fn new<L: DatasetCacheLookup>(lookup: L) -> Self {
        Self {
            lookup: Arc::new(lookup),
        }
    }

    pub fn from_arc(lookup: Arc<dyn DatasetCacheLookup>) -> Self {
        Self { lookup }
    }
}

impl std::fmt::Debug for GetSampleTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GetSampleTool").finish_non_exhaustive()
    }
}

#[async_trait]
impl ToolHandler for GetSampleTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: format!(
                "Draw up to `n` rows from a cached dataset using `head` (deterministic, \
                 first n rows in storage order), `random` (reservoir-sampled, optional \
                 `seed` for reproducibility), or `stratified` (requires `stratify_col`; \
                 `ceil(n / k)` per group, trimmed to `n`). Specify the dataset by `id` \
                 or `slug`; default n = {DEFAULT_N}, capped at {MAX_N}. The scan is \
                 bounded by {MAX_SAMPLE_SCAN_ROWS} rows — `sampled: true` flags \
                 responses where the underlying file was larger."
            ),
            input_schema: input_schema(),
            output_schema: Some(output_schema()),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let req = Request::parse(&args)?;
        let cache = self
            .lookup
            .dataset_cache(req.key.clone())
            .await
            .map_err(|e| ToolError::Execution(format!("storage: {e}")))?;
        let Some(cache) = cache else {
            return Err(ToolError::NotFound(format!(
                "dataset not found ({})",
                req.lookup_str(),
            )));
        };

        let parquet_path = parquet_path_for_sample(&cache)?;
        let slug = cache.slug.clone();
        let strategy = req.strategy.clone();
        let n = req.n;

        // `DatasetEngine::scan` + sampling are blocking — same
        // pattern as describe_schema. Timeout note: the
        // tokio::time::timeout is caller-side only; the
        // spawn_blocking task continues running until natural
        // completion (DESIGN.md §6 worker-process isolation
        // tracked separately).
        let work = tokio::task::spawn_blocking(move || {
            draw_sample(&parquet_path, n, &strategy, MAX_SAMPLE_SCAN_ROWS)
        });

        match tokio::time::timeout(SAMPLE_TIMEOUT, work).await {
            Ok(Ok(Ok(report))) => Ok(report.render()),
            Ok(Ok(Err(SampleError::BadArgument(msg)))) => {
                // Caller-controlled mistake — surface verbatim so the
                // agent can correct the request. The BadArgument
                // variant is constructed only from caller-supplied
                // column names, so it doesn't leak server paths.
                Err(ToolError::InvalidArguments(msg))
            }
            Ok(Ok(Err(e))) => {
                tracing::warn!(
                    slug = %slug,
                    sample_error = %e,
                    "get_sample draw failed",
                );
                Err(ToolError::Execution(
                    "sample draw failed — see server logs for details".into(),
                ))
            }
            Ok(Err(join_err)) => {
                tracing::error!(
                    slug = %slug,
                    join_error = %join_err,
                    "get_sample worker join failed",
                );
                Err(ToolError::Execution(
                    "sample worker crashed unexpectedly".into(),
                ))
            }
            Err(_) => Err(ToolError::Execution(format!(
                "sample draw exceeded {}s deadline",
                SAMPLE_TIMEOUT.as_secs(),
            ))),
        }
    }
}

#[derive(Debug, Error)]
enum SampleError {
    /// Upstream engine failure (scan or collect). Preserves the
    /// engine's stable op-label contract verbatim.
    #[error("{0}")]
    Engine(#[from] EngineError),
    /// Caller-controlled mistake — e.g. asking to stratify on a
    /// column the dataset doesn't have. Maps to
    /// `ToolError::InvalidArguments` in the async caller so the
    /// agent sees an actionable message rather than a generic
    /// "see server logs". The string carries only column names
    /// from the caller's own request, never file paths or schema
    /// internals.
    #[error("get_sample: {0}")]
    BadArgument(String),
    /// Polars / partition / sample call returned a shape the tool
    /// didn't expect. Treated as a server-side error and logged
    /// rather than surfaced verbatim so internal drift doesn't
    /// leak through.
    #[error("get_sample: {0}")]
    Internal(String),
}

/// Worker-side helper. Returns a `SampleReport` ready to render.
fn draw_sample(
    path: &Path,
    n: u32,
    strategy: &SampleStrategy,
    scan_cap: u32,
) -> Result<SampleReport, SampleError> {
    // Probe scan_cap + 1 so we can detect "underlying dataset is
    // larger than the scan cap" without doing an extra scan. Same
    // pattern as describe_schema's introspect_parquet.
    let probe_limit = scan_cap.saturating_add(1);
    let lf = DatasetEngine::scan(
        DatasetSource::Parquet(path),
        &LoadOptions {
            projection: None,
            row_limit: Some(probe_limit),
        },
    )?;
    let probed = DatasetEngine::collect(lf)?;
    let scan_cap_usize = scan_cap as usize;
    let sampled = probed.height() > scan_cap_usize;
    let scanned = if sampled {
        probed.head(Some(scan_cap_usize))
    } else {
        probed
    };
    let scan_height = scanned.height();

    let n_usize = n as usize;
    let drawn = match strategy {
        SampleStrategy::Head => {
            // `head(Some(n))` clamps internally if n > height.
            scanned.head(Some(n_usize))
        }
        SampleStrategy::Random { seed } => {
            if scan_height == 0 {
                scanned.clone()
            } else {
                // Polars 0.53 sample_n signature:
                //   fn sample_n(&self, n: &Series, with_replacement: bool,
                //               shuffle: bool, seed: Option<u64>) -> PolarsResult<DataFrame>
                // We cap n at the scan height so we never ask for more
                // rows than exist (Polars otherwise errors out without
                // replacement). shuffle=true so head/tail isn't biased
                // when seed isn't supplied.
                let effective_n = n_usize.min(scan_height) as u64;
                let n_series = Series::new(PlSmallStr::from_static("n"), &[effective_n]);
                scanned
                    .sample_n(&n_series, false, true, *seed)
                    .map_err(|e| SampleError::Internal(format!("random sample_n failed: {e}")))?
            }
        }
        SampleStrategy::Stratified { stratify_col } => {
            stratified_sample(&scanned, stratify_col, n_usize)?
        }
    };

    Ok(SampleReport {
        strategy_wire: strategy.as_wire(),
        requested_n: n,
        rows: drawn,
        sampled,
        scan_cap,
    })
}

/// Stratified sampling: partition by `stratify_col`, take `ceil(n/k)`
/// per group (where `k` = distinct strata count, but ≤ n so empty
/// groups don't claim slots), reservoir-sample within each group,
/// then trim the concatenated frame to `n`. When the dataset has
/// more strata than `n`, we sample from only the first `k` groups
/// (`partition_by` with `maintain_order=true` gives a deterministic
/// "first" by storage order) — sampling every group would blow up
/// memory + time without contributing more than `n` rows to the
/// final result.
fn stratified_sample(
    scanned: &DataFrame,
    stratify_col: &str,
    n: usize,
) -> Result<DataFrame, SampleError> {
    if n == 0 || scanned.height() == 0 {
        return Ok(scanned.head(Some(0)));
    }
    // Pre-check column existence so a caller mistake surfaces as
    // BadArgument (→ InvalidArguments) rather than a generic
    // server-side error. partition_by would otherwise bubble up a
    // Polars message that's not actionable for the agent.
    if scanned.column(stratify_col).is_err() {
        return Err(SampleError::BadArgument(format!(
            "stratify_col `{stratify_col}` is not a column of the dataset",
        )));
    }
    let partitions = scanned
        .partition_by([stratify_col], true)
        .map_err(|e| SampleError::Internal(format!("partition_by(`{stratify_col}`): {e}")))?;
    if partitions.is_empty() {
        return Ok(scanned.head(Some(0)));
    }
    // `k = min(distinct_strata, n)` so when there are more groups
    // than `n` rows we don't compute a per-group share of 0 (which
    // would yield an empty result for non-empty input). Iterate
    // only the first `k` partitions — sampling every group would
    // do work proportional to the distinct-value count without
    // contributing more than `n` rows to the final result.
    let k = partitions.len().min(n);
    let per_group = n.div_ceil(k);

    let mut chunks: Vec<DataFrame> = Vec::with_capacity(k);
    for part in partitions.into_iter().take(k) {
        let cap = per_group.min(part.height());
        if cap == 0 {
            continue;
        }
        let cap_u64 = cap as u64;
        let n_series = Series::new(PlSmallStr::from_static("n"), &[cap_u64]);
        let sampled = part
            .sample_n(&n_series, false, true, None)
            .map_err(|e| SampleError::Internal(format!("stratified sample_n: {e}")))?;
        chunks.push(sampled);
    }
    if chunks.is_empty() {
        return Ok(scanned.head(Some(0)));
    }
    // Fold the per-group samples via `vstack`. Polars' DataFrame
    // concat helpers (`concat_df` and friends) moved between 0.5x
    // releases and the lazy `concat` family wants `LazyFrame`s;
    // hand-rolling the fold keeps the bound type stable and is just
    // a few lines for the small `partitions` count.
    let mut chunks_iter = chunks.into_iter();
    let mut combined = chunks_iter
        .next()
        .expect("guarded by `chunks.is_empty()` above");
    for next in chunks_iter {
        combined
            .vstack_mut(&next)
            .map_err(|e| SampleError::Internal(format!("vstack strata: {e}")))?;
    }
    if combined.height() > n {
        combined = combined.head(Some(n));
    }
    Ok(combined)
}

struct SampleReport {
    strategy_wire: &'static str,
    requested_n: u32,
    rows: DataFrame,
    sampled: bool,
    scan_cap: u32,
}

impl SampleReport {
    fn render(&self) -> Value {
        // Render exactly like `query_rows`: columns then rows. Keeps
        // the agent-facing shape consistent across rich tools.
        let columns: Vec<Value> = self
            .rows
            .get_column_names()
            .iter()
            .map(|c| Value::String(c.to_string()))
            .collect();
        let df_columns = self.rows.columns();
        let height = self.rows.height();
        let mut rows: Vec<Value> = Vec::with_capacity(height);
        for row_idx in 0..height {
            let mut row = Vec::with_capacity(df_columns.len());
            for column in df_columns {
                row.push(any_value_to_json(
                    &column.get(row_idx).unwrap_or(AnyValue::Null),
                ));
            }
            rows.push(Value::Array(row));
        }
        json!({
            "strategy": self.strategy_wire,
            "requested_n": self.requested_n,
            "returned": height,
            "columns": columns,
            "rows": rows,
            "sampled": self.sampled,
            "scan_cap": self.scan_cap,
        })
    }
}

/// Best-effort `AnyValue` → JSON conversion. Same shape as
/// `query_rows::any_value_to_json` and `describe_schema::any_value_to_json`.
fn any_value_to_json(av: &AnyValue<'_>) -> Value {
    match av {
        AnyValue::Null => Value::Null,
        AnyValue::Boolean(b) => Value::Bool(*b),
        AnyValue::String(s) => Value::String((*s).to_string()),
        AnyValue::StringOwned(s) => Value::String(s.to_string()),
        AnyValue::Int8(n) => json!(*n),
        AnyValue::Int16(n) => json!(*n),
        AnyValue::Int32(n) => json!(*n),
        AnyValue::Int64(n) => json!(*n),
        AnyValue::UInt8(n) => json!(*n),
        AnyValue::UInt16(n) => json!(*n),
        AnyValue::UInt32(n) => json!(*n),
        AnyValue::UInt64(n) => json!(*n),
        AnyValue::Float32(n) if n.is_finite() => json!(*n),
        AnyValue::Float64(n) if n.is_finite() => json!(*n),
        AnyValue::Float32(n) => Value::String(format!("{n}")),
        AnyValue::Float64(n) => Value::String(format!("{n}")),
        other => Value::String(format!("{other}")),
    }
}

fn parquet_path_for_sample(cache: &CacheRef) -> Result<PathBuf, ToolError> {
    let (true, Some(raw)) = (cache.cached, cache.cache_path.as_deref()) else {
        return Err(ToolError::NotFound(format!(
            "dataset `{}` is not materialised yet — call `materialize_dataset` first",
            cache.slug,
        )));
    };
    if let Some(stripped) = raw.strip_prefix("file://") {
        Ok(PathBuf::from(stripped))
    } else if let Some(scheme) = raw.split_once("://").map(|(s, _)| s) {
        tracing::warn!(
            slug = %cache.slug,
            cache_scheme = %scheme,
            "cache uri scheme not yet supported by get_sample",
        );
        Err(ToolError::Execution(format!(
            "cache scheme `{scheme}` is not yet supported by get_sample"
        )))
    } else {
        Ok(PathBuf::from(raw))
    }
}

struct Request {
    key: DatasetKey,
    lookup_repr: String,
    strategy: SampleStrategy,
    n: u32,
}

impl Request {
    fn parse(args: &Value) -> Result<Self, ToolError> {
        let obj = args
            .as_object()
            .ok_or_else(|| ToolError::InvalidArguments("arguments must be a JSON object".into()))?;

        let id = optional_string(obj, "id")?
            .map(|s| Uuid::parse_str(&s))
            .transpose()
            .map_err(|e| ToolError::InvalidArguments(format!("`id` is not a valid UUID: {e}")))?;
        let slug = optional_string(obj, "slug")?;
        let (key, lookup_repr) = match (id, slug) {
            (Some(id), None) => (DatasetKey::Id(id), format!("id={id}")),
            (None, Some(slug)) => {
                let repr = format!("slug={slug}");
                (DatasetKey::Slug(slug), repr)
            }
            (None, None) => {
                return Err(ToolError::InvalidArguments(
                    "exactly one of `id` or `slug` is required".into(),
                ));
            }
            (Some(_), Some(_)) => {
                return Err(ToolError::InvalidArguments(
                    "only one of `id` or `slug` may be specified".into(),
                ));
            }
        };

        let n = parse_n(obj)?;
        let strategy_wire = optional_string(obj, "strategy")?.unwrap_or_else(|| "head".into());
        let stratify_col = optional_string(obj, "stratify_col")?;
        let seed = parse_seed(obj)?;

        let strategy = match strategy_wire.as_str() {
            "head" => {
                if stratify_col.is_some() {
                    return Err(ToolError::InvalidArguments(
                        "`stratify_col` is only valid with strategy=\"stratified\"".into(),
                    ));
                }
                if seed.is_some() {
                    return Err(ToolError::InvalidArguments(
                        "`seed` is only valid with strategy=\"random\"".into(),
                    ));
                }
                SampleStrategy::Head
            }
            "random" => {
                if stratify_col.is_some() {
                    return Err(ToolError::InvalidArguments(
                        "`stratify_col` is only valid with strategy=\"stratified\"".into(),
                    ));
                }
                SampleStrategy::Random { seed }
            }
            "stratified" => {
                if seed.is_some() {
                    return Err(ToolError::InvalidArguments(
                        "`seed` is only valid with strategy=\"random\"".into(),
                    ));
                }
                let col = stratify_col.ok_or_else(|| {
                    ToolError::InvalidArguments(
                        "`stratified` strategy requires `stratify_col`".into(),
                    )
                })?;
                if col.is_empty() {
                    return Err(ToolError::InvalidArguments(
                        "`stratify_col` must be a non-empty string".into(),
                    ));
                }
                SampleStrategy::Stratified { stratify_col: col }
            }
            other => {
                return Err(ToolError::InvalidArguments(format!(
                    "`strategy` must be one of {ACCEPTED_STRATEGIES:?}, got {other:?}"
                )));
            }
        };

        Ok(Self {
            key,
            lookup_repr,
            strategy,
            n,
        })
    }

    fn lookup_str(&self) -> &str {
        &self.lookup_repr
    }
}

fn parse_n(obj: &Map<String, Value>) -> Result<u32, ToolError> {
    match obj.get("n") {
        None | Some(Value::Null) => Ok(DEFAULT_N),
        Some(Value::Number(num)) => {
            let n_u64 = num.as_u64().ok_or_else(|| {
                ToolError::InvalidArguments(format!(
                    "`n` must be a non-negative integer ≤ {MAX_N}, got {num}"
                ))
            })?;
            let n_u32 = u32::try_from(n_u64).map_err(|_| {
                ToolError::InvalidArguments(format!("`n` must be ≤ {MAX_N}, got {num}"))
            })?;
            if n_u32 > MAX_N {
                Err(ToolError::InvalidArguments(format!(
                    "`n` capped at {MAX_N}, got {n_u32}"
                )))
            } else {
                Ok(n_u32)
            }
        }
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`n` must be a non-negative integer, got {}",
            kind_of(other)
        ))),
    }
}

fn parse_seed(obj: &Map<String, Value>) -> Result<Option<u64>, ToolError> {
    match obj.get("seed") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(num)) => num
            .as_u64()
            .map(Some)
            .ok_or_else(|| ToolError::InvalidArguments(format!("`seed` must be a u64, got {num}"))),
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`seed` must be an integer, got {}",
            kind_of(other)
        ))),
    }
}

fn optional_string(obj: &Map<String, Value>, key: &str) -> Result<Option<String>, ToolError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`{key}` must be a string, got {}",
            kind_of(other),
        ))),
    }
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn input_schema() -> Map<String, Value> {
    let n_description = format!(
        "Number of rows requested. Capped at {MAX_N}; the response may return fewer when the dataset itself is smaller."
    );
    json!({
        "type": "object",
        "properties": {
            "id": {
                "type": "string",
                "format": "uuid",
                "description": "Dataset UUID. Exactly one of `id` or `slug` is required.",
            },
            "slug": {
                "type": "string",
                "description": "Marketplace slug. Exactly one of `id` or `slug` is required.",
            },
            "strategy": {
                "type": "string",
                "enum": ["head", "random", "stratified"],
                "default": "head",
                "description": "Sampling strategy. `stratified` requires `stratify_col`; `seed` is only valid for `random`.",
            },
            "n": {
                "type": "integer",
                "minimum": 0,
                "maximum": MAX_N,
                "default": DEFAULT_N,
                "description": n_description,
            },
            "stratify_col": {
                "type": "string",
                "description": "Column name to stratify by. Required when strategy=\"stratified\", rejected otherwise.",
            },
            "seed": {
                "type": "integer",
                "minimum": 0,
                "description": "Optional u64 seed for random sampling. Only valid when strategy=\"random\"; rejected on head / stratified.",
            },
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled input schema must be an object")
}

fn output_schema() -> Map<String, Value> {
    let scan_cap_description =
        format!("Row cap the engine applied to the scan — {MAX_SAMPLE_SCAN_ROWS} rows.");
    json!({
        "type": "object",
        "required": ["strategy", "requested_n", "returned", "columns", "rows", "sampled", "scan_cap"],
        "properties": {
            "strategy": { "type": "string", "enum": ["head", "random", "stratified"] },
            "requested_n": { "type": "integer", "minimum": 0 },
            "returned": { "type": "integer", "minimum": 0, "description": "Actual number of rows drawn. ≤ requested_n; smaller when the dataset has fewer rows than requested." },
            "columns": { "type": "array", "items": { "type": "string" } },
            "rows": { "type": "array", "items": { "type": "array" } },
            "sampled": { "type": "boolean", "description": "True when the underlying dataset has more rows than `scan_cap`." },
            "scan_cap": { "type": "integer", "minimum": 1, "description": scan_cap_description },
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled output schema must be an object")
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use polars::prelude::{ParquetWriter, df};
    use storage::StorageError;
    use tempfile::TempDir;

    #[derive(Clone)]
    struct StubLookup {
        response: Arc<Mutex<Option<CacheRef>>>,
    }

    impl StubLookup {
        fn new(response: Option<CacheRef>) -> Self {
            Self {
                response: Arc::new(Mutex::new(response)),
            }
        }
    }

    #[async_trait]
    impl DatasetCacheLookup for StubLookup {
        async fn dataset_cache(&self, _key: DatasetKey) -> Result<Option<CacheRef>, StorageError> {
            Ok(self.response.lock().unwrap().clone())
        }
    }

    fn write_fixture_parquet() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("fixture.parquet");
        // 10 rows; `group` has 2 distinct values (3:7 split) so
        // stratified sampling has something to chew on.
        let mut df = df! {
            "id" => &(1_i64..=10).collect::<Vec<_>>(),
            "group" => &["a", "b", "a", "b", "b", "a", "b", "b", "b", "b"],
        }
        .expect("build df");
        let file = std::fs::File::create(&path).expect("create parquet");
        ParquetWriter::new(file).finish(&mut df).expect("write");
        (dir, path)
    }

    fn cache_ref_for(path: &Path) -> CacheRef {
        CacheRef {
            id: Uuid::nil(),
            slug: "fixture".into(),
            cached: true,
            cache_path: Some(path.to_string_lossy().into_owned()),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn head_default_returns_first_n_in_storage_order() {
        let (_g, path) = write_fixture_parquet();
        let tool = GetSampleTool::new(StubLookup::new(Some(cache_ref_for(&path))));
        let out = tool
            .call(json!({"slug": "fixture", "n": 3}))
            .await
            .expect("ok");
        assert_eq!(out["strategy"], "head");
        assert_eq!(out["returned"], 3);
        let rows = out["rows"].as_array().unwrap();
        // id is column 0; first three are 1, 2, 3 in storage order.
        let ids: Vec<i64> = rows.iter().map(|r| r[0].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn default_n_is_ten() {
        let (_g, path) = write_fixture_parquet();
        let tool = GetSampleTool::new(StubLookup::new(Some(cache_ref_for(&path))));
        let out = tool.call(json!({"slug": "fixture"})).await.expect("ok");
        assert_eq!(out["requested_n"], 10);
        assert_eq!(out["returned"], 10);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn n_above_max_is_rejected() {
        let (_g, path) = write_fixture_parquet();
        let tool = GetSampleTool::new(StubLookup::new(Some(cache_ref_for(&path))));
        let err = tool
            .call(json!({"slug": "fixture", "n": 5000}))
            .await
            .expect_err("over cap");
        match err {
            ToolError::InvalidArguments(m) => assert!(m.contains("1000"), "got: {m}"),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn random_with_seed_is_reproducible() {
        let (_g, path) = write_fixture_parquet();
        let tool = GetSampleTool::new(StubLookup::new(Some(cache_ref_for(&path))));
        let a = tool
            .call(json!({"slug": "fixture", "strategy": "random", "n": 4, "seed": 42}))
            .await
            .expect("ok");
        let b = tool
            .call(json!({"slug": "fixture", "strategy": "random", "n": 4, "seed": 42}))
            .await
            .expect("ok");
        // Same seed ⇒ same rows. Compare row arrays directly.
        assert_eq!(a["rows"], b["rows"]);
        assert_eq!(a["returned"], 4);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn random_without_replacement_returns_distinct_ids() {
        let (_g, path) = write_fixture_parquet();
        let tool = GetSampleTool::new(StubLookup::new(Some(cache_ref_for(&path))));
        let out = tool
            .call(json!({"slug": "fixture", "strategy": "random", "n": 5, "seed": 7}))
            .await
            .expect("ok");
        let rows = out["rows"].as_array().unwrap();
        let mut ids: Vec<i64> = rows.iter().map(|r| r[0].as_i64().unwrap()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 5, "sample without replacement → all distinct");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stratified_returns_rows_from_each_group() {
        let (_g, path) = write_fixture_parquet();
        let tool = GetSampleTool::new(StubLookup::new(Some(cache_ref_for(&path))));
        let out = tool
            .call(json!({
                "slug": "fixture",
                "strategy": "stratified",
                "stratify_col": "group",
                "n": 4,
            }))
            .await
            .expect("ok");
        assert_eq!(out["strategy"], "stratified");
        // 2 groups, n=4 ⇒ per_group=2, total returned=4.
        assert_eq!(out["returned"], 4);
        // Collect the group values from column index 1.
        let rows = out["rows"].as_array().unwrap();
        let groups: Vec<&str> = rows.iter().map(|r| r[1].as_str().unwrap()).collect();
        assert!(groups.contains(&"a"));
        assert!(groups.contains(&"b"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stratified_requires_stratify_col() {
        let (_g, path) = write_fixture_parquet();
        let tool = GetSampleTool::new(StubLookup::new(Some(cache_ref_for(&path))));
        let err = tool
            .call(json!({"slug": "fixture", "strategy": "stratified"}))
            .await
            .expect_err("missing stratify_col");
        match err {
            ToolError::InvalidArguments(m) => assert!(m.contains("stratify_col"), "got: {m}"),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stratify_col_rejected_for_non_stratified_strategies() {
        let (_g, path) = write_fixture_parquet();
        let tool = GetSampleTool::new(StubLookup::new(Some(cache_ref_for(&path))));
        let err = tool
            .call(json!({"slug": "fixture", "strategy": "head", "stratify_col": "group"}))
            .await
            .expect_err("stratify_col on head");
        match err {
            ToolError::InvalidArguments(m) => assert!(m.contains("stratified"), "got: {m}"),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn seed_rejected_for_non_random_strategies() {
        let (_g, path) = write_fixture_parquet();
        let tool = GetSampleTool::new(StubLookup::new(Some(cache_ref_for(&path))));
        let err = tool
            .call(json!({"slug": "fixture", "strategy": "head", "seed": 1}))
            .await
            .expect_err("seed on head");
        match err {
            ToolError::InvalidArguments(m) => assert!(m.contains("random"), "got: {m}"),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_strategy_rejected() {
        let (_g, path) = write_fixture_parquet();
        let tool = GetSampleTool::new(StubLookup::new(Some(cache_ref_for(&path))));
        let err = tool
            .call(json!({"slug": "fixture", "strategy": "cluster"}))
            .await
            .expect_err("unknown");
        match err {
            ToolError::InvalidArguments(m) => assert!(m.contains("strategy"), "got: {m}"),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    /// Locks R1's `BadArgument` routing: a `stratify_col` that
    /// doesn't exist on the dataset surfaces as `InvalidArguments`
    /// (caller-fixable) rather than the generic "see server logs"
    /// execution error.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stratified_unknown_column_returns_invalid_arguments() {
        let (_g, path) = write_fixture_parquet();
        let tool = GetSampleTool::new(StubLookup::new(Some(cache_ref_for(&path))));
        let err = tool
            .call(json!({
                "slug": "fixture",
                "strategy": "stratified",
                "stratify_col": "nope",
            }))
            .await
            .expect_err("nonexistent col");
        match err {
            ToolError::InvalidArguments(m) => {
                assert!(m.contains("nope"), "got: {m}");
                assert!(m.contains("stratify_col"), "got: {m}");
            }
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_cache_returns_not_found_with_materialize_hint() {
        let tool = GetSampleTool::new(StubLookup::new(Some(CacheRef {
            id: Uuid::nil(),
            slug: "fixture".into(),
            cached: false,
            cache_path: None,
        })));
        let err = tool
            .call(json!({"slug": "fixture"}))
            .await
            .expect_err("not materialised");
        match err {
            ToolError::NotFound(m) => assert!(m.contains("materialize_dataset"), "got: {m}"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_dataset_returns_not_found() {
        let tool = GetSampleTool::new(StubLookup::new(None));
        let err = tool
            .call(json!({"slug": "no-such-thing"}))
            .await
            .expect_err("unknown");
        assert!(matches!(err, ToolError::NotFound(_)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unsupported_cache_scheme_leaks_only_scheme() {
        let tool = GetSampleTool::new(StubLookup::new(Some(CacheRef {
            id: Uuid::nil(),
            slug: "fixture".into(),
            cached: true,
            cache_path: Some("s3://secret-bucket/key.parquet?sig=AAA".into()),
        })));
        let err = tool
            .call(json!({"slug": "fixture"}))
            .await
            .expect_err("unsupported scheme");
        match err {
            ToolError::Execution(m) => {
                assert!(m.contains("s3"), "scheme should appear: {m}");
                assert!(!m.contains("secret-bucket"), "bucket leak: {m}");
                assert!(!m.contains("sig=AAA"), "signed-URL leak: {m}");
            }
            other => panic!("expected Execution, got {other:?}"),
        }
    }

    #[test]
    fn descriptor_advertises_input_and_output_schemas() {
        let d = GetSampleTool::new(StubLookup::new(None)).descriptor();
        assert_eq!(d.name, "get_sample");
        assert!(d.output_schema.is_some());
    }
}
