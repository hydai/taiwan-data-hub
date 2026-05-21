//! `describe_schema` MCP tool — introspects the schema of a cached
//! dataset's Parquet via [`mcp_core::DatasetEngine`] and returns
//! column-level metadata for downstream agent reasoning.
//!
//! Per the #3.2 Definition of Done the response carries, per column:
//!  - `name` and `dtype` (Polars type name as a stable wire string)
//!  - `nullable` boolean
//!  - `sample_values`: first 5 non-null values, JSON-encoded
//!  - `approx_distinct_count`: `HyperLogLog++` estimate from Polars'
//!    `approx_n_unique` expression — exact for small datasets,
//!    approximate (typically <2% error) for large ones
//!  - `description`: column-level business description, when
//!    available from the storage layer (today: always `null`; a
//!    `column_metadata` table is a follow-up — DESIGN.md §4.3)
//!
//! The tool caps work at [`MAX_SAMPLE_ROWS`] via
//! [`mcp_core::LoadOptions::row_limit`] so a 100M-row dataset
//! doesn't OOM the worker; the response flags `sampled: true` when
//! the cap clipped the scan so callers know the distinct count is
//! "approx over the first N rows" rather than "approx over the
//! whole table". Bound chosen so the HLL estimate stays accurate
//! without forcing the full table through memory.

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

/// Failures the blocking introspection helper can surface. Kept
/// separate from [`EngineError`] so this tool's tool-level invariant
/// violations don't masquerade as Polars output — `EngineError::Polars`
/// has a documented stable contract (`<op>[ (<path>)]: <polars message>`
/// where `op` is one of `scan parquet`, `scan csv`, `scan ndjson`,
/// `collect`) and overloading it for "polars returned a cell shape we
/// didn't expect" would corrupt log/grep patterns downstream consumers
/// rely on.
#[derive(Debug, Error)]
enum IntrospectError {
    /// Upstream engine failure (scan or collect). Preserves the
    /// engine's own message verbatim so the stable op-label contract
    /// flows through.
    #[error("{0}")]
    Engine(#[from] EngineError),
    /// Polars' output didn't match the shape `describe_schema` expects
    /// (missing column in an aggregation frame, empty cell where a
    /// single u64 was promised, an `AnyValue` variant we haven't been
    /// taught about). Indicates a contract drift, not an upstream
    /// engine error.
    #[error("describe_schema: {0}")]
    UnexpectedShape(String),
}

/// MCP tool name. Stable identifier — clients pin to this string.
pub const TOOL_NAME: &str = "describe_schema";

/// Row cap applied to the scan that feeds both the sample-value
/// pick and the HLL distinct count. 100k matches what the Polars
/// HLL implementation needs for its sub-2% error bound on typical
/// data; going higher costs memory without improving the estimate.
pub const MAX_SAMPLE_ROWS: u32 = 100_000;

/// Number of non-null values surfaced per column in the response.
/// Five matches the issue's Definition of Done; rendered in row
/// order, no random sample.
pub const SAMPLE_VALUE_COUNT: usize = 5;

/// Per-call deadline. Deliberately higher than `query_rows`'s 5s
/// cap because `describe_schema` runs *per-column* work (sample
/// materialise + HLL pass) over up to `MAX_SAMPLE_ROWS` rows;
/// wider tables can approach the cap legitimately. An accidentally
/// huge schema scan still cannot tie up the blocking pool
/// indefinitely.
const SCHEMA_TIMEOUT: Duration = Duration::from_secs(10);

/// Reads from any [`DatasetCacheLookup`]; production wires a
/// `storage::Storage`, tests plug in an in-memory stub.
#[derive(Clone)]
pub struct DescribeSchemaTool {
    lookup: Arc<dyn DatasetCacheLookup>,
}

impl DescribeSchemaTool {
    pub fn new<L: DatasetCacheLookup>(lookup: L) -> Self {
        Self {
            lookup: Arc::new(lookup),
        }
    }

    pub fn from_arc(lookup: Arc<dyn DatasetCacheLookup>) -> Self {
        Self { lookup }
    }
}

impl std::fmt::Debug for DescribeSchemaTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DescribeSchemaTool").finish_non_exhaustive()
    }
}

#[async_trait]
impl ToolHandler for DescribeSchemaTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: format!(
                "Inspect a cached dataset's columns: dtype, nullability, sample values, \
                 and approximate distinct count (HyperLogLog++). Specify the dataset by \
                 `id` (UUID) or `slug`; exactly one is required. Stats and sample values \
                 are computed over at most {MAX_SAMPLE_ROWS} rows (the implementation \
                 reads one extra row internally to disambiguate \"exactly cap rows\" from \
                 \"clipped\"); `sampled: true` flags responses where the underlying \
                 dataset has more rows than the cap."
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

        let parquet_path = parquet_path_for_schema(&cache)?;
        let slug = cache.slug.clone();

        // `DatasetEngine::scan` + `collect` are blocking — move to a
        // dedicated blocking task and bound the wallclock the same
        // way `query_rows` does.
        //
        // **Timeout limitation**: `tokio::time::timeout` is a
        // caller-side deadline only. Dropping the `JoinHandle` does
        // not preempt an OS thread, so a Polars scan that overruns
        // the deadline keeps running on the blocking pool until it
        // naturally completes — the *caller* gets the deadline
        // error but resources aren't reclaimed. The MAX_SAMPLE_ROWS
        // cap plus the bounded per-column work keep the worst-case
        // wallclock predictable; a hard kill needs worker-process
        // isolation (DESIGN.md §6, tracked separately, same as the
        // identical caveat in `query_rows`).
        let work =
            tokio::task::spawn_blocking(move || introspect_parquet(&parquet_path, MAX_SAMPLE_ROWS));

        match tokio::time::timeout(SCHEMA_TIMEOUT, work).await {
            Ok(Ok(Ok(report))) => Ok(report.render()),
            Ok(Ok(Err(e))) => {
                // IntrospectError covers both upstream Engine failures
                // (which carry the cache path + raw Polars context) and
                // local invariant violations from `approx_distinct_
                // from_cell`. Log full server-side, return a sanitised
                // public message — same pattern as `query_rows`.
                tracing::warn!(
                    slug = %slug,
                    introspect_error = %e,
                    "describe_schema introspection failed",
                );
                Err(ToolError::Execution(
                    "schema introspection failed — see server logs for details".into(),
                ))
            }
            Ok(Err(join_err)) => {
                tracing::error!(
                    slug = %slug,
                    join_error = %join_err,
                    "describe_schema worker join failed",
                );
                Err(ToolError::Execution(
                    "schema worker crashed unexpectedly".into(),
                ))
            }
            Err(_) => Err(ToolError::Execution(format!(
                "schema introspection exceeded {}s deadline",
                SCHEMA_TIMEOUT.as_secs(),
            ))),
        }
    }
}

/// Whatever the blocking worker pulls out of Polars. Renders to
/// JSON via [`ColumnReport::render`] on the async side so the
/// JSON building doesn't need to know about the lazy plan.
struct SchemaReport {
    columns: Vec<ColumnReport>,
    /// Number of rows the engine actually inspected — the smaller of
    /// the dataset size and [`MAX_SAMPLE_ROWS`].
    row_count: usize,
    /// True iff the underlying dataset has *more* than `MAX_SAMPLE_ROWS`
    /// rows (detected by the +1 probe in `introspect_parquet`). False
    /// when the dataset fits within the cap exactly or under it; in
    /// that case the stats are authoritative for the whole table.
    sampled: bool,
}

impl SchemaReport {
    fn render(&self) -> Value {
        let cols: Vec<Value> = self.columns.iter().map(ColumnReport::render).collect();
        json!({
            "columns": cols,
            "row_count": self.row_count,
            "sampled": self.sampled,
            "sample_cap": MAX_SAMPLE_ROWS,
        })
    }
}

struct ColumnReport {
    name: String,
    /// Polars `DataType` Display string (e.g. `"i64"`, `"str"`,
    /// `"List[i64]"`). Polars' type names are stable across patch
    /// releases per the upstream changelog; we expose them as the
    /// wire form so agents can pattern-match without us having to
    /// maintain a translation table.
    dtype: String,
    /// Polars columns are nominally nullable, but a column with
    /// zero observed nulls in the sampled window is reported as
    /// `nullable: false` so agents have an actionable hint. The
    /// flag is *sample-derived* — `sampled: true` at the top level
    /// flags that the answer is not authoritative for the whole
    /// table.
    nullable: bool,
    sample_values: Vec<Value>,
    approx_distinct_count: u64,
}

impl ColumnReport {
    fn render(&self) -> Value {
        json!({
            "name": self.name,
            "dtype": self.dtype,
            "nullable": self.nullable,
            "sample_values": self.sample_values,
            "approx_distinct_count": self.approx_distinct_count,
            // Column-level descriptions need a `column_metadata`
            // table in Postgres that doesn't exist yet (see DESIGN
            // §4.3 follow-up). Field is present with `null` so the
            // wire shape is stable for the PG-side patch.
            "description": Value::Null,
        })
    }
}

/// Blocking helper that runs on `spawn_blocking`. Scans, collects,
/// and per-column computes sample + `approx_distinct`.
///
/// Probes `row_cap + 1` rows so we can disambiguate "dataset is
/// exactly `row_cap` rows" from "dataset was clipped to `row_cap`":
/// the former returns ≤ `row_cap` rows; the latter returns
/// `row_cap + 1`. Stats and sample values are then computed over the
/// first `row_cap` rows of the resulting frame.
fn introspect_parquet(path: &Path, row_cap: u32) -> Result<SchemaReport, IntrospectError> {
    let probe_limit = row_cap.saturating_add(1);
    let lf = DatasetEngine::scan(
        DatasetSource::Parquet(path),
        &LoadOptions {
            projection: None,
            row_limit: Some(probe_limit),
        },
    )?;
    let probed = DatasetEngine::collect(lf)?;
    let cap_usize = row_cap as usize;
    let sampled = probed.height() > cap_usize;
    let df = if sampled {
        probed.head(Some(cap_usize))
    } else {
        probed
    };

    // Compute approx distinct per column in a single lazy pass so
    // Polars can fold the aggregations together. The result is a
    // 1-row frame with one u64 column per input column.
    let approx_lf = df.clone().lazy().select(
        df.get_column_names()
            .iter()
            .map(|n| {
                let name: &str = n.as_str();
                col(name).approx_n_unique().alias(name)
            })
            .collect::<Vec<_>>(),
    );
    let approx_frame = DatasetEngine::collect(approx_lf)?;

    // Hoist `df.columns()` out of the loop — it returns a borrow
    // that's stable for the loop's lifetime, so calling per
    // iteration just adds bounds-check overhead. Matches the same
    // pattern in `query_rows::render_dataframe`.
    let df_columns = df.columns();
    let mut columns = Vec::with_capacity(df_columns.len());
    for column in df_columns {
        let name = column.name().to_string();
        let dtype = format!("{}", column.dtype());
        let nullable = column.null_count() > 0;

        // Sample values: iterate row-by-row and short-circuit once
        // SAMPLE_VALUE_COUNT non-null values are found. The earlier
        // `column.drop_nulls()` materialised an entire non-null
        // Series per column — wasteful for wide tables (up to 100k
        // rows × N columns) when we only need 5 values. The
        // iterator path allocates one Vec of at most 5 entries.
        let mut sample_values = Vec::with_capacity(SAMPLE_VALUE_COUNT);
        for i in 0..column.len() {
            if sample_values.len() == SAMPLE_VALUE_COUNT {
                break;
            }
            let cell = column.get(i).unwrap_or(AnyValue::Null);
            if !matches!(cell, AnyValue::Null) {
                sample_values.push(any_value_to_json(&cell));
            }
        }

        // approx_n_unique returns one cell per column. Polars
        // surfaces it as UInt32/UInt64 normally, occasionally as
        // Int64 from the lazy-plan fallback on small inputs. Surface
        // every other shape as an EngineError so a Polars contract
        // shift doesn't silently masquerade as "0 distinct values".
        let approx_distinct_count = approx_distinct_from_cell(&approx_frame, &name)?;

        columns.push(ColumnReport {
            name,
            dtype,
            nullable,
            sample_values,
            approx_distinct_count,
        });
    }

    Ok(SchemaReport {
        columns,
        row_count: df.height(),
        sampled,
    })
}

/// Extract the `approx_n_unique` result from the 1-row aggregation
/// frame Polars returns. Every unexpected shape — missing column,
/// empty result, negative integer, or a dtype the engine hasn't been
/// taught about — surfaces as [`IntrospectError::UnexpectedShape`]
/// so contract drifts don't silently degrade to "0 distinct values"
/// downstream *and* don't pollute the `EngineError::Polars` op-label
/// contract from #3.1.
fn approx_distinct_from_cell(frame: &DataFrame, col_name: &str) -> Result<u64, IntrospectError> {
    let column = frame.column(col_name).map_err(|_| {
        IntrospectError::UnexpectedShape(format!(
            "approx_n_unique result missing column `{col_name}`",
        ))
    })?;
    let cell = column.get(0).map_err(|_| {
        IntrospectError::UnexpectedShape(format!(
            "approx_n_unique returned empty result for column `{col_name}`",
        ))
    })?;
    match cell {
        AnyValue::UInt32(n) => Ok(u64::from(n)),
        AnyValue::UInt64(n) => Ok(n),
        AnyValue::Int64(n) => u64::try_from(n).map_err(|_| {
            IntrospectError::UnexpectedShape(format!(
                "approx_n_unique returned negative Int64 ({n}) for column `{col_name}`",
            ))
        }),
        AnyValue::Int32(n) => u64::try_from(n).map_err(|_| {
            IntrospectError::UnexpectedShape(format!(
                "approx_n_unique returned negative Int32 ({n}) for column `{col_name}`",
            ))
        }),
        // Empty / all-null columns yield 0 distinct non-nulls; map
        // Polars' Null cell to 0 explicitly so this case stays
        // intentional rather than silently caught by the catch-all.
        AnyValue::Null => Ok(0),
        other => Err(IntrospectError::UnexpectedShape(format!(
            "approx_n_unique returned unexpected type for column `{col_name}`: {other}",
        ))),
    }
}

/// Resolve the file-system path Polars should scan. Mirrors
/// `query_rows::parquet_path_for_query` so cache-scheme handling
/// stays consistent across rich tools.
fn parquet_path_for_schema(cache: &CacheRef) -> Result<PathBuf, ToolError> {
    let (true, Some(raw)) = (cache.cached, cache.cache_path.as_deref()) else {
        return Err(ToolError::NotFound(format!(
            "dataset `{}` is not materialised yet — call `materialize_dataset` first",
            cache.slug,
        )));
    };

    if let Some(stripped) = raw.strip_prefix("file://") {
        Ok(PathBuf::from(stripped))
    } else if let Some(scheme) = raw.split_once("://").map(|(s, _)| s) {
        // Echo only the scheme back; the full URI may carry bucket
        // names / signed-URL params we don't want leaking out of the
        // server.
        tracing::warn!(
            slug = %cache.slug,
            cache_scheme = %scheme,
            "cache uri scheme not yet supported by describe_schema",
        );
        Err(ToolError::Execution(format!(
            "cache scheme `{scheme}` is not yet supported by describe_schema"
        )))
    } else {
        Ok(PathBuf::from(raw))
    }
}

/// Best-effort conversion from a Polars cell to JSON. Same shape as
/// `query_rows::any_value_to_json` — kept inline rather than shared
/// because the two tools may diverge (e.g. `query_rows` may render
/// numbers differently from a schema-introspection viewer in
/// future).
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

struct Request {
    key: DatasetKey,
    lookup_repr: String,
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

        match (id, slug) {
            (Some(id), None) => Ok(Self {
                key: DatasetKey::Id(id),
                lookup_repr: format!("id={id}"),
            }),
            (None, Some(slug)) => {
                let repr = format!("slug={slug}");
                Ok(Self {
                    key: DatasetKey::Slug(slug),
                    lookup_repr: repr,
                })
            }
            (None, None) => Err(ToolError::InvalidArguments(
                "exactly one of `id` or `slug` is required".into(),
            )),
            (Some(_), Some(_)) => Err(ToolError::InvalidArguments(
                "only one of `id` or `slug` may be specified".into(),
            )),
        }
    }

    fn lookup_str(&self) -> &str {
        &self.lookup_repr
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
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled input schema must be an object")
}

fn output_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["columns", "row_count", "sampled", "sample_cap"],
        "properties": {
            "columns": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["name", "dtype", "nullable", "sample_values", "approx_distinct_count", "description"],
                    "properties": {
                        "name": { "type": "string" },
                        "dtype": { "type": "string", "description": "Polars data type name (e.g. `i64`, `str`, `List[i64]`)." },
                        "nullable": { "type": "boolean", "description": "Sample-derived: false if the sampled window had zero nulls." },
                        "sample_values": {
                            "type": "array",
                            "description": "First 5 non-null values in row order.",
                        },
                        "approx_distinct_count": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "HyperLogLog++ estimate over the sampled rows (typically <2% error).",
                        },
                        "description": {
                            "type": ["string", "null"],
                            "description": "Column-level business description from storage. `null` today; populated once the `column_metadata` table lands (DESIGN.md §4.3 follow-up).",
                        },
                    },
                    "additionalProperties": false,
                },
            },
            "row_count": {
                "type": "integer",
                "minimum": 0,
                "description": "Number of rows the engine used to compute stats and sample values (always ≤ `sample_cap`). When `sampled=true` the underlying dataset has *more* rows than this — `row_count` is not the total file size.",
            },
            "sampled": {
                "type": "boolean",
                "description": "True when the scan was clipped at `sample_cap`. Distinct counts and nullability flags are then sample-derived, not authoritative for the full table.",
            },
            "sample_cap": {
                "type": "integer",
                "minimum": 1,
                "description": "Row cap the engine applied — see the `MAX_SAMPLE_ROWS` module const.",
            },
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled output schema must be an object")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
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
        let mut df = df! {
            "id" => &[1_i64, 2, 3, 4, 5, 6],
            // `name` includes a duplicate so distinct < height.
            "name" => &["a", "b", "a", "c", "d", "e"],
            // Optional column with one null in the middle, plus the
            // null-leading prefix the sample-values code has to skip.
            "score" => &[None, None, Some(3.0_f64), Some(4.0), None, Some(6.0)],
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
    async fn happy_path_reports_columns_dtypes_and_samples() {
        let (_guard, path) = write_fixture_parquet();
        let tool = DescribeSchemaTool::new(StubLookup::new(Some(cache_ref_for(&path))));

        let out = tool
            .call(json!({"slug": "fixture"}))
            .await
            .expect("call ok");

        assert_eq!(out["row_count"], 6);
        assert_eq!(out["sampled"], false, "6 rows fits well under the cap");
        let cols = out["columns"].as_array().unwrap();
        assert_eq!(cols.len(), 3);

        let id_col = &cols[0];
        assert_eq!(id_col["name"], "id");
        assert_eq!(id_col["dtype"], "i64");
        assert_eq!(id_col["nullable"], false);
        // `id` has no nulls and is fully distinct, so the sample is
        // the first 5 rows and approx_distinct == 6.
        assert_eq!(id_col["sample_values"].as_array().unwrap().len(), 5);
        assert_eq!(id_col["approx_distinct_count"], 6);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nullable_column_flagged_and_sample_skips_leading_nulls() {
        let (_guard, path) = write_fixture_parquet();
        let tool = DescribeSchemaTool::new(StubLookup::new(Some(cache_ref_for(&path))));

        let out = tool
            .call(json!({"slug": "fixture"}))
            .await
            .expect("call ok");

        let score = out["columns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "score")
            .expect("score column present");
        assert_eq!(score["nullable"], true);
        // `score` fixture is [null, null, 3.0, 4.0, null, 6.0] — the
        // three non-null values must surface in row order, leading
        // nulls skipped.
        let samples: Vec<f64> = score["sample_values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().expect("number"))
            .collect();
        assert_eq!(samples, vec![3.0, 4.0, 6.0]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn duplicate_values_reduce_approx_distinct_count() {
        let (_guard, path) = write_fixture_parquet();
        let tool = DescribeSchemaTool::new(StubLookup::new(Some(cache_ref_for(&path))));

        let out = tool
            .call(json!({"slug": "fixture"}))
            .await
            .expect("call ok");

        // `name` is [a, b, a, c, d, e] → 5 distinct values out of 6
        // rows. The Polars HLL implementation is exact for cardinality
        // this small, so the count is a hard 5.
        let name = out["columns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "name")
            .expect("name column present");
        assert_eq!(name["approx_distinct_count"], 5);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn description_field_is_null_until_pg_metadata_lands() {
        let (_guard, path) = write_fixture_parquet();
        let tool = DescribeSchemaTool::new(StubLookup::new(Some(cache_ref_for(&path))));

        let out = tool
            .call(json!({"slug": "fixture"}))
            .await
            .expect("call ok");
        for col in out["columns"].as_array().unwrap() {
            assert!(
                col["description"].is_null(),
                "description is null pending column_metadata table: {col}",
            );
        }
    }

    /// Locks the R1 disambiguation: a dataset whose row count
    /// *exactly* equals the engine cap must report `sampled: false`,
    /// not `true`. The +1 probe in `introspect_parquet` is what makes
    /// this possible — without it a 100k-row dataset would look
    /// identical to a clipped 100k+ one. Uses a per-test cap injected
    /// via the private helper so we don't have to build a 100k-row
    /// fixture.
    ///
    /// Plain `#[test]` (not `#[tokio::test]`) because the helper is
    /// synchronous; Polars' `collect` spins up its own runtime and
    /// would panic with "cannot start a runtime from within a
    /// runtime" if we wrapped this in a Tokio context.
    #[test]
    fn dataset_exactly_at_cap_reports_sampled_false() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("at-cap.parquet");
        let mut df = df! {
            "id" => &(0_i64..6).collect::<Vec<_>>(),
        }
        .expect("build df");
        let file = std::fs::File::create(&path).expect("create");
        ParquetWriter::new(file).finish(&mut df).expect("write");

        // 6-row dataset, 5-row cap → +1 probe sees 6 rows, sampled=true.
        let report_above = introspect_parquet(&path, 5).expect("introspect");
        assert_eq!(report_above.row_count, 5, "stats run over first cap rows");
        assert!(
            report_above.sampled,
            "6 rows > 5 cap must surface as sampled=true",
        );

        // 6-row dataset, 6-row cap → +1 probe sees 6 rows (no extra),
        // so we know the dataset fits exactly. sampled=false.
        let report_at = introspect_parquet(&path, 6).expect("introspect");
        assert_eq!(report_at.row_count, 6);
        assert!(
            !report_at.sampled,
            "exact-cap dataset must surface as sampled=false",
        );

        // 6-row dataset, 10-row cap → +1 probe sees 6 rows (under cap),
        // so we know the dataset is smaller than the cap. sampled=false.
        let report_under = introspect_parquet(&path, 10).expect("introspect");
        assert_eq!(report_under.row_count, 6);
        assert!(!report_under.sampled);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_cache_returns_not_found_with_materialize_hint() {
        let tool = DescribeSchemaTool::new(StubLookup::new(Some(CacheRef {
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
            ToolError::NotFound(msg) => {
                assert!(msg.contains("materialize_dataset"), "got: {msg}");
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_dataset_returns_not_found() {
        let tool = DescribeSchemaTool::new(StubLookup::new(None));
        let err = tool
            .call(json!({"slug": "no-such-thing"}))
            .await
            .expect_err("unknown");
        assert!(matches!(err, ToolError::NotFound(_)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_lookup_args_rejected() {
        let tool = DescribeSchemaTool::new(StubLookup::new(None));
        let err = tool.call(json!({})).await.expect_err("no id/slug");
        match err {
            ToolError::InvalidArguments(m) => assert!(m.contains("exactly one"), "got: {m}"),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn both_id_and_slug_rejected() {
        let tool = DescribeSchemaTool::new(StubLookup::new(None));
        let err = tool
            .call(json!({
                "id": Uuid::nil().to_string(),
                "slug": "fixture",
            }))
            .await
            .expect_err("both id and slug");
        match err {
            ToolError::InvalidArguments(m) => assert!(m.contains("only one"), "got: {m}"),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unsupported_cache_scheme_leaks_only_scheme() {
        let tool = DescribeSchemaTool::new(StubLookup::new(Some(CacheRef {
            id: Uuid::nil(),
            slug: "fixture".into(),
            cached: true,
            cache_path: Some("s3://secret-bucket-internal/key.parquet?signature=AAA".into()),
        })));

        let err = tool
            .call(json!({"slug": "fixture"}))
            .await
            .expect_err("unsupported scheme");
        match err {
            ToolError::Execution(m) => {
                assert!(m.contains("s3"), "scheme name should appear: {m}");
                assert!(!m.contains("secret-bucket-internal"), "bucket leak: {m}");
                assert!(!m.contains("signature"), "signed-URL leak: {m}");
            }
            other => panic!("expected Execution, got {other:?}"),
        }
    }

    #[test]
    fn descriptor_advertises_input_and_output_schemas() {
        let d = DescribeSchemaTool::new(StubLookup::new(None)).descriptor();
        assert_eq!(d.name, "describe_schema");
        assert!(d.output_schema.is_some());
    }
}
