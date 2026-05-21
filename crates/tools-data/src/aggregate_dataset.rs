//! `aggregate_dataset` MCP tool — group-by + aggregate (sum / mean /
//! count / median / min / max / stddev) on a cached dataset via
//! `DatasetEngine`'s lazy pipeline. Refuses results with more than
//! [`MAX_GROUPS`] distinct groups.
//!
//! Per the #3.5 Definition of Done:
//!  - `group_by`: `string[]` (one or more column names).
//!  - `agg`: array of `{col, fn}` specs; `fn` is one of
//!    `sum` / `mean` / `count` / `median` / `min` / `max` / `stddev`.
//!  - Output columns are named `<col>_<fn>` (e.g. `amount_sum`); the
//!    grouping columns appear under their original names.
//!  - Refuses when the aggregated table would exceed `MAX_GROUPS`
//!    rows — agents getting a refusal should pre-filter or pick a
//!    coarser grouping.
//!
//! Per-side scan capped at [`MAX_SCAN_ROWS`] (100k, matching the
//! other rich tools); `sampled: true` flags responses where the
//! underlying dataset is larger.

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

pub const TOOL_NAME: &str = "aggregate_dataset";

/// Maximum number of distinct groups the aggregation can produce.
/// Bigger than this and the result becomes hard for an agent to
/// reason about; the tool refuses rather than truncating silently.
pub const MAX_GROUPS: u32 = 100_000;

/// Row cap on the scan that feeds the aggregation. Matches
/// `describe_schema` / `get_sample` / `join_datasets`.
pub const MAX_SCAN_ROWS: u32 = 100_000;

/// Per-call deadline. Same as `join_datasets`: a 100k-row group-by
/// with seven aggregations is comparable workload.
const AGGREGATE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AggFn {
    Sum,
    Mean,
    Count,
    Median,
    Min,
    Max,
    Stddev,
}

impl AggFn {
    fn as_wire(self) -> &'static str {
        match self {
            Self::Sum => "sum",
            Self::Mean => "mean",
            Self::Count => "count",
            Self::Median => "median",
            Self::Min => "min",
            Self::Max => "max",
            Self::Stddev => "stddev",
        }
    }

    fn from_wire(s: &str) -> Option<Self> {
        match s {
            "sum" => Some(Self::Sum),
            "mean" => Some(Self::Mean),
            "count" => Some(Self::Count),
            "median" => Some(Self::Median),
            "min" => Some(Self::Min),
            "max" => Some(Self::Max),
            "stddev" => Some(Self::Stddev),
            _ => None,
        }
    }

    /// Build the Polars expression for this aggregation on the
    /// given column. The alias makes the output column name
    /// `<col>_<fn>` so the response is self-describing and two
    /// aggregations on the same column don't collide.
    fn to_expr(self, col_name: &str) -> Expr {
        let target = col(col_name);
        let alias_name = format!("{col_name}_{}", self.as_wire());
        match self {
            Self::Sum => target.sum(),
            Self::Mean => target.mean(),
            // Polars' `count()` skips nulls; for a "total row count
            // per group" use `len()` instead. The DoD's `count`
            // matches the SQL semantic, so non-null count is what
            // we want.
            Self::Count => target.count(),
            Self::Median => target.median(),
            Self::Min => target.min(),
            Self::Max => target.max(),
            // Sample stddev (ddof=1) so a single-row group yields
            // null rather than 0 — matches the SQL `STDDEV_SAMP`
            // convention agents typically expect.
            Self::Stddev => target.std(1),
        }
        .alias(alias_name.as_str())
    }
}

const ACCEPTED_FNS: &[&str] = &["sum", "mean", "count", "median", "min", "max", "stddev"];

#[derive(Debug, Clone)]
struct AggSpec {
    col: String,
    agg_fn: AggFn,
}

#[derive(Clone)]
pub struct AggregateDatasetTool {
    lookup: Arc<dyn DatasetCacheLookup>,
}

impl AggregateDatasetTool {
    pub fn new<L: DatasetCacheLookup>(lookup: L) -> Self {
        Self {
            lookup: Arc::new(lookup),
        }
    }

    pub fn from_arc(lookup: Arc<dyn DatasetCacheLookup>) -> Self {
        Self { lookup }
    }
}

impl std::fmt::Debug for AggregateDatasetTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AggregateDatasetTool")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ToolHandler for AggregateDatasetTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: format!(
                "Group a cached dataset by one or more columns and compute aggregations \
                 (sum / mean / count / median / min / max / stddev). Specify the dataset \
                 by `id` or `slug`. Output columns are named `<col>_<fn>` so two \
                 aggregations on the same column don't collide. Refuses when the \
                 aggregated table would exceed {MAX_GROUPS} groups — pre-filter or \
                 pick a coarser grouping. Scan is capped at {MAX_SCAN_ROWS} rows; \
                 `sampled: true` flags responses where the dataset is larger."
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
        let parquet_path = parquet_path_for_aggregate(&cache)?;
        let slug = cache.slug.clone();
        let group_by = req.group_by.clone();
        let agg_specs = req.aggs.clone();
        let page = req.page;
        let page_size = req.page_size;

        let work = tokio::task::spawn_blocking(move || {
            run_aggregate(
                &parquet_path,
                &group_by,
                &agg_specs,
                MAX_SCAN_ROWS,
                MAX_GROUPS,
                page,
                page_size,
            )
        });

        match tokio::time::timeout(AGGREGATE_TIMEOUT, work).await {
            Ok(Ok(Ok(report))) => Ok(report.render()),
            Ok(Ok(Err(AggError::BadArgument {
                message,
                underlying,
            }))) => {
                tracing::debug!(
                    slug = %slug,
                    underlying = ?underlying,
                    "aggregate_dataset BadArgument underlying engine error",
                );
                tracing::info!(
                    slug = %slug,
                    bad_argument = %message,
                    "aggregate_dataset rejected user request",
                );
                Err(ToolError::InvalidArguments(message))
            }
            Ok(Ok(Err(AggError::TooManyGroups { cap }))) => {
                Err(ToolError::InvalidArguments(format!(
                    "aggregation would produce more than {cap} groups; pre-filter or \
                     pick a coarser grouping",
                )))
            }
            Ok(Ok(Err(e))) => {
                tracing::warn!(slug = %slug, agg_error = %e, "aggregate_dataset failed");
                Err(ToolError::Execution(
                    "aggregation failed — see server logs for details".into(),
                ))
            }
            Ok(Err(join_err)) => {
                tracing::error!(slug = %slug, join_error = %join_err, "aggregate_dataset worker join failed");
                Err(ToolError::Execution(
                    "aggregation worker crashed unexpectedly".into(),
                ))
            }
            Err(_) => Err(ToolError::Execution(format!(
                "aggregation exceeded {}s deadline",
                AGGREGATE_TIMEOUT.as_secs(),
            ))),
        }
    }
}

#[derive(Debug, Error)]
enum AggError {
    #[error("{0}")]
    Engine(#[from] EngineError),
    #[error("aggregate_dataset: {message}")]
    BadArgument {
        message: String,
        underlying: Option<EngineError>,
    },
    #[error("aggregation produced more than {cap} groups")]
    TooManyGroups { cap: u32 },
}

#[allow(clippy::too_many_arguments)]
fn run_aggregate(
    path: &Path,
    group_by: &[String],
    aggs: &[AggSpec],
    scan_cap: u32,
    group_cap: u32,
    page: u32,
    page_size: u32,
) -> Result<AggReport, AggError> {
    // Probe scan_cap+1 so sampled flag reflects "underlying dataset
    // larger than the scan cap" — same +1 pattern as the other
    // rich tools.
    let scan_probe = scan_cap.saturating_add(1);
    let probed = DatasetEngine::scan(
        DatasetSource::Parquet(path),
        &LoadOptions {
            projection: None,
            row_limit: Some(scan_probe),
        },
    )?;
    let scan_count_frame = DatasetEngine::collect(probed.clone().select([len()]))?;
    let scan_height = parse_single_count(&scan_count_frame, "scan probe")?;
    let sampled = scan_height > u64::from(scan_cap);
    let scanned_lf = probed.limit(scan_cap);

    let group_exprs: Vec<Expr> = group_by.iter().map(|c| col(c.as_str())).collect();
    let agg_exprs: Vec<Expr> = aggs.iter().map(|a| a.agg_fn.to_expr(&a.col)).collect();
    let cap_plus_one = group_cap.saturating_add(1);
    let aggregated_lf = scanned_lf
        .group_by(group_exprs)
        .agg(agg_exprs)
        .limit(cap_plus_one);

    // Pass 1: count distinct groups. select([len()]) collapses to
    // one cell; the lazy plan stops aggregating after cap+1.
    let group_count_frame = DatasetEngine::collect(aggregated_lf.clone().select([len()]))
        .map_err(|e| classify(e, group_by, aggs))?;
    let total_groups_probe = parse_single_count(&group_count_frame, "group count probe")?;
    if total_groups_probe > u64::from(group_cap) {
        return Err(AggError::TooManyGroups { cap: group_cap });
    }
    let total_groups =
        usize::try_from(total_groups_probe.min(u64::from(group_cap))).unwrap_or(usize::MAX);

    // Honest accounting: when the input is sampled (the dataset is
    // larger than the scan cap), the group count we just computed
    // covers only the first `scan_cap` rows. The cap was respected
    // for what we saw but *might not* hold on the full table. Flag
    // it so callers don't trust the total as authoritative.
    let groups_partial_due_to_sampling = sampled;

    // Pass 2: collect only the requested page. .slice(offset,
    // page_size) is folded into the lazy plan.
    let offset = i64::from(page.saturating_sub(1)).saturating_mul(i64::from(page_size));
    let page_lf = aggregated_lf.limit(group_cap).slice(offset, page_size);
    let collected_page =
        DatasetEngine::collect(page_lf).map_err(|e| classify(e, group_by, aggs))?;

    Ok(AggReport {
        rows: collected_page,
        total_groups,
        page,
        page_size,
        sampled,
        groups_partial_due_to_sampling,
        scan_cap,
        group_cap,
    })
}

/// Decode the single `len()` cell — same pattern as `join_datasets`.
fn parse_single_count(frame: &DataFrame, context: &str) -> Result<u64, AggError> {
    let column = frame
        .column("len")
        .map_err(|_| EngineError::Polars(format!("{context} missing `len` column")))?;
    let cell = column
        .get(0)
        .map_err(|_| EngineError::Polars(format!("{context} returned empty result")))?;
    match cell {
        AnyValue::UInt32(n) => Ok(u64::from(n)),
        AnyValue::UInt64(n) => Ok(n),
        AnyValue::Int64(n) => u64::try_from(n).map_err(|_| {
            EngineError::Polars(format!("{context} returned negative Int64 ({n})")).into()
        }),
        AnyValue::Int32(n) => u64::try_from(n).map_err(|_| {
            EngineError::Polars(format!("{context} returned negative Int32 ({n})")).into()
        }),
        other => {
            Err(EngineError::Polars(format!("{context} returned unexpected type: {other}")).into())
        }
    }
}

fn classify(err: EngineError, group_by: &[String], aggs: &[AggSpec]) -> AggError {
    let EngineError::Polars(ref msg) = err else {
        return AggError::Engine(err);
    };
    // Collect all referenced column names and check whether the
    // error mentions any of them with a missing-column signature.
    let mut referenced: Vec<&str> = group_by.iter().map(String::as_str).collect();
    referenced.extend(aggs.iter().map(|a| a.col.as_str()));
    let mentions_col = referenced.iter().any(|c| msg.contains(c));
    let mentions_missing = msg.contains("not found")
        || msg.contains("ColumnNotFound")
        || msg.contains("unable to find");
    if mentions_col && mentions_missing {
        let agg_cols: Vec<&str> = aggs.iter().map(|a| a.col.as_str()).collect();
        AggError::BadArgument {
            message: format!(
                "one of the referenced columns is not in the dataset: \
                 group_by={group_by:?}, agg_cols={agg_cols:?}",
            ),
            underlying: Some(err),
        }
    } else {
        AggError::Engine(err)
    }
}

#[derive(Debug)]
struct AggReport {
    rows: DataFrame,
    /// Total groups in the aggregation (capped at `group_cap`).
    total_groups: usize,
    page: u32,
    page_size: u32,
    sampled: bool,
    /// True when `sampled=true` — signals that the group count and
    /// the per-group aggregates are over only the first `scan_cap`
    /// rows, not the full dataset.
    groups_partial_due_to_sampling: bool,
    scan_cap: u32,
    group_cap: u32,
}

impl AggReport {
    fn render(&self) -> Value {
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
        let page_size_usize = (self.page_size as usize).max(1);
        let total_pages_usize = self.total_groups.div_ceil(page_size_usize).max(1);
        let total_pages = u32::try_from(total_pages_usize).unwrap_or(u32::MAX);
        json!({
            "page": self.page,
            "page_size": self.page_size,
            "total_pages": total_pages,
            "total_groups": self.total_groups,
            "columns": columns,
            "rows": rows,
            "sampled": self.sampled,
            "groups_partial_due_to_sampling": self.groups_partial_due_to_sampling,
            "scan_cap": self.scan_cap,
            "group_cap": self.group_cap,
        })
    }
}

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

fn parquet_path_for_aggregate(cache: &CacheRef) -> Result<PathBuf, ToolError> {
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
            cache_path_redacted = %redact_uri_for_log(raw),
            "cache uri scheme not yet supported by aggregate_dataset",
        );
        Err(ToolError::Execution(format!(
            "cache scheme `{scheme}` is not yet supported by aggregate_dataset"
        )))
    } else {
        Ok(PathBuf::from(raw))
    }
}

fn redact_uri_for_log(uri: &str) -> String {
    let head = uri.split_once('?').map_or(uri, |(head, _)| head);
    let head = head.split_once('#').map_or(head, |(head, _)| head);
    head.to_owned()
}

/// Default page size for the aggregation response. Smaller than
/// `MAX_GROUPS` so a 100k-group result naturally paginates.
pub const DEFAULT_PAGE_SIZE: u32 = 100;
pub const MAX_PAGE_SIZE: u32 = 1_000;

struct Request {
    key: DatasetKey,
    lookup_repr: String,
    group_by: Vec<String>,
    aggs: Vec<AggSpec>,
    page: u32,
    page_size: u32,
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

        let group_by = parse_group_by(obj)?;
        let parsed_aggs = parse_aggs(obj)?;
        let page = parse_positive_u32(obj, "page", 1, u32::MAX)?;
        let page_size = parse_positive_u32(obj, "page_size", DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE)?;
        Ok(Self {
            key,
            lookup_repr,
            group_by,
            aggs: parsed_aggs,
            page,
            page_size,
        })
    }

    fn lookup_str(&self) -> &str {
        &self.lookup_repr
    }
}

fn parse_group_by(obj: &Map<String, Value>) -> Result<Vec<String>, ToolError> {
    let v = obj.get("group_by").ok_or_else(|| {
        ToolError::InvalidArguments("`group_by` is required (array of column names)".into())
    })?;
    let arr = v.as_array().ok_or_else(|| {
        ToolError::InvalidArguments(format!("`group_by` must be an array, got {}", kind_of(v)))
    })?;
    if arr.is_empty() {
        return Err(ToolError::InvalidArguments(
            "`group_by` must list at least one column".into(),
        ));
    }
    let mut out = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        match item {
            Value::String(s) if !s.is_empty() => out.push(s.clone()),
            Value::String(_) => {
                return Err(ToolError::InvalidArguments(format!(
                    "`group_by[{idx}]` must be a non-empty column name"
                )));
            }
            other => {
                return Err(ToolError::InvalidArguments(format!(
                    "`group_by[{idx}]` must be a string, got {}",
                    kind_of(other)
                )));
            }
        }
    }
    Ok(out)
}

fn parse_aggs(obj: &Map<String, Value>) -> Result<Vec<AggSpec>, ToolError> {
    let v = obj.get("agg").ok_or_else(|| {
        ToolError::InvalidArguments("`agg` is required (array of {col, fn} objects)".into())
    })?;
    let arr = v.as_array().ok_or_else(|| {
        ToolError::InvalidArguments(format!("`agg` must be an array, got {}", kind_of(v)))
    })?;
    if arr.is_empty() {
        return Err(ToolError::InvalidArguments(
            "`agg` must list at least one aggregation".into(),
        ));
    }
    let mut out = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let spec_obj = item.as_object().ok_or_else(|| {
            ToolError::InvalidArguments(format!(
                "`agg[{idx}]` must be an object with `col` and `fn`, got {}",
                kind_of(item)
            ))
        })?;
        let col = optional_string(spec_obj, "col")?.ok_or_else(|| {
            ToolError::InvalidArguments(format!(
                "`agg[{idx}].col` is required (non-empty column name)"
            ))
        })?;
        let fn_wire = optional_string(spec_obj, "fn")?
            .ok_or_else(|| ToolError::InvalidArguments(format!("`agg[{idx}].fn` is required")))?;
        let agg_fn = AggFn::from_wire(&fn_wire).ok_or_else(|| {
            ToolError::InvalidArguments(format!(
                "`agg[{idx}].fn` must be one of {ACCEPTED_FNS:?}, got {fn_wire:?}"
            ))
        })?;
        out.push(AggSpec { col, agg_fn });
    }
    Ok(out)
}

fn parse_positive_u32(
    obj: &Map<String, Value>,
    key: &str,
    default: u32,
    max: u32,
) -> Result<u32, ToolError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Number(num)) => {
            let n = num.as_u64().ok_or_else(|| {
                ToolError::InvalidArguments(format!(
                    "`{key}` must be a positive integer ≤ {max}, got {num}"
                ))
            })?;
            let n_u32 = u32::try_from(n).map_err(|_| {
                ToolError::InvalidArguments(format!("`{key}` must be ≤ {max}, got {num}"))
            })?;
            if n_u32 == 0 {
                Err(ToolError::InvalidArguments(format!(
                    "`{key}` must be a positive integer"
                )))
            } else if n_u32 > max {
                Err(ToolError::InvalidArguments(format!(
                    "`{key}` must be ≤ {max}, got {n_u32}"
                )))
            } else {
                Ok(n_u32)
            }
        }
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`{key}` must be a positive integer, got {}",
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
    let top_level_description = format!(
        "Specify the dataset by `id` (UUID) or `slug`; exactly one is required. \
         Refuses when aggregation would yield more than {MAX_GROUPS} groups; \
         pre-filter the dataset or pick a coarser grouping."
    );
    json!({
        "type": "object",
        "required": ["group_by", "agg"],
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
            "group_by": {
                "type": "array",
                "items": { "type": "string", "minLength": 1 },
                "minItems": 1,
                "description": "Column names to group by. Output preserves these as ordinary columns alongside the aggregates.",
            },
            "agg": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "required": ["col", "fn"],
                    "properties": {
                        "col": { "type": "string", "minLength": 1, "description": "Column to aggregate." },
                        "fn": { "type": "string", "enum": ACCEPTED_FNS, "description": "Aggregation function." },
                    },
                    "additionalProperties": false,
                },
                "description": "List of aggregations. Output columns are named `<col>_<fn>` so two specs on the same column don't collide.",
            },
            "page": {
                "type": "integer",
                "minimum": 1,
                "default": 1,
                "description": "1-based page number into the aggregation result.",
            },
            "page_size": {
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_PAGE_SIZE,
                "default": DEFAULT_PAGE_SIZE,
                "description": "Rows per page (each row is one group).",
            },
        },
        "additionalProperties": false,
        "description": top_level_description,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled input schema must be an object")
}

fn output_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["page", "page_size", "total_pages", "total_groups", "columns", "rows", "sampled", "groups_partial_due_to_sampling", "scan_cap", "group_cap"],
        "properties": {
            "page": { "type": "integer", "minimum": 1 },
            "page_size": { "type": "integer", "minimum": 1 },
            "total_pages": { "type": "integer", "minimum": 1 },
            "total_groups": { "type": "integer", "minimum": 0, "description": "Total distinct groups in the result (≤ group_cap)." },
            "columns": { "type": "array", "items": { "type": "string" } },
            "rows": { "type": "array", "items": { "type": "array" } },
            "sampled": { "type": "boolean", "description": "True when the underlying dataset has more rows than scan_cap." },
            "groups_partial_due_to_sampling": { "type": "boolean", "description": "True when the input was sampled (sampled=true) — the group count and per-group aggregates cover only the first scan_cap rows. The full table may have more groups." },
            "scan_cap": { "type": "integer", "minimum": 1 },
            "group_cap": { "type": "integer", "minimum": 1, "description": "Maximum groups the tool will materialise." },
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

    fn write_sales() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("sales.parquet");
        // 2 regions × 3 rows each so group_by yields predictable
        // sums / counts. amount=NaN omitted on purpose so stddev
        // stays finite on the small fixture.
        let mut df = df! {
            "region" => &["a", "a", "a", "b", "b", "b"],
            "amount" => &[10_f64, 20.0, 30.0, 5.0, 7.0, 9.0],
        }
        .expect("build df");
        let file = std::fs::File::create(&path).expect("create");
        ParquetWriter::new(file).finish(&mut df).expect("write");
        (dir, path)
    }

    fn cache_ref_for(path: &Path) -> CacheRef {
        CacheRef {
            id: Uuid::nil(),
            slug: "sales".into(),
            cached: true,
            cache_path: Some(path.to_string_lossy().into_owned()),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sum_per_group_returns_expected_totals() {
        let (_d, path) = write_sales();
        let tool = AggregateDatasetTool::new(StubLookup::new(Some(cache_ref_for(&path))));
        let out = tool
            .call(json!({
                "slug": "sales",
                "group_by": ["region"],
                "agg": [{"col": "amount", "fn": "sum"}],
            }))
            .await
            .expect("ok");
        assert_eq!(out["total_groups"], 2);
        let columns: Vec<&str> = out["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(columns.contains(&"region"));
        assert!(columns.contains(&"amount_sum"));
        // Rows aren't ordered by Polars group_by; collect into a map.
        let region_idx = columns.iter().position(|c| *c == "region").unwrap();
        let sum_idx = columns.iter().position(|c| *c == "amount_sum").unwrap();
        let rows = out["rows"].as_array().unwrap();
        let mut by_region = std::collections::HashMap::new();
        for row in rows {
            let r = row[region_idx].as_str().unwrap();
            let s = row[sum_idx].as_f64().unwrap();
            by_region.insert(r.to_string(), s);
        }
        // Use absolute-difference comparisons — clippy refuses
        // strict eq on floats, and the f64 sums above are exact in
        // IEEE-754 (powers of 2 + small integers), so an ε of 0 is
        // technically fine; using 1e-9 is conservative-and-clear.
        assert!((by_region["a"] - 60.0).abs() < 1e-9);
        assert!((by_region["b"] - 21.0).abs() < 1e-9);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn multiple_aggs_emit_distinct_columns() {
        let (_d, path) = write_sales();
        let tool = AggregateDatasetTool::new(StubLookup::new(Some(cache_ref_for(&path))));
        let out = tool
            .call(json!({
                "slug": "sales",
                "group_by": ["region"],
                "agg": [
                    {"col": "amount", "fn": "sum"},
                    {"col": "amount", "fn": "mean"},
                    {"col": "amount", "fn": "count"},
                ],
            }))
            .await
            .expect("ok");
        let columns: Vec<&str> = out["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        // Three aggregates on the same column → three distinct output
        // columns thanks to the alias scheme.
        assert!(columns.contains(&"amount_sum"));
        assert!(columns.contains(&"amount_mean"));
        assert!(columns.contains(&"amount_count"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_fn_rejected() {
        let (_d, path) = write_sales();
        let tool = AggregateDatasetTool::new(StubLookup::new(Some(cache_ref_for(&path))));
        let err = tool
            .call(json!({
                "slug": "sales",
                "group_by": ["region"],
                "agg": [{"col": "amount", "fn": "geomean"}],
            }))
            .await
            .expect_err("unknown fn");
        match err {
            ToolError::InvalidArguments(m) => assert!(m.contains("geomean"), "got: {m}"),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_group_by_column_returns_invalid_arguments() {
        let (_d, path) = write_sales();
        let tool = AggregateDatasetTool::new(StubLookup::new(Some(cache_ref_for(&path))));
        let err = tool
            .call(json!({
                "slug": "sales",
                "group_by": ["nope"],
                "agg": [{"col": "amount", "fn": "sum"}],
            }))
            .await
            .expect_err("missing group_by col");
        match err {
            ToolError::InvalidArguments(m) => assert!(m.contains("nope"), "got: {m}"),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_group_by_rejected() {
        let tool = AggregateDatasetTool::new(StubLookup::new(None));
        let err = tool
            .call(json!({"slug": "sales", "agg": [{"col": "amount", "fn": "sum"}]}))
            .await
            .expect_err("missing group_by");
        match err {
            ToolError::InvalidArguments(m) => assert!(m.contains("group_by"), "got: {m}"),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_agg_rejected() {
        let tool = AggregateDatasetTool::new(StubLookup::new(None));
        let err = tool
            .call(json!({"slug": "sales", "group_by": ["region"]}))
            .await
            .expect_err("missing agg");
        match err {
            ToolError::InvalidArguments(m) => assert!(m.contains("agg"), "got: {m}"),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    /// Locks the group-cardinality cap. Build a fixture with 5
    /// distinct groups and use a per-test cap of 3 to trigger the
    /// refusal. Plain `#[test]` because `run_aggregate` is sync.
    #[test]
    fn too_many_groups_rejects() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("many.parquet");
        let mut df = df! {
            "g" => &["a", "b", "c", "d", "e", "a"],
            "v" => &[1_i64, 2, 3, 4, 5, 6],
        }
        .expect("build df");
        let file = std::fs::File::create(&path).expect("create");
        ParquetWriter::new(file).finish(&mut df).expect("write");

        let aggs = vec![AggSpec {
            col: "v".into(),
            agg_fn: AggFn::Sum,
        }];
        // 5 distinct groups > cap=3 → refusal.
        let err = run_aggregate(&path, &["g".into()], &aggs, 100, 3, 1, 100).expect_err("over cap");
        match err {
            AggError::TooManyGroups { cap } => assert_eq!(cap, 3),
            other => panic!("expected TooManyGroups, got {other:?}"),
        }

        // 5 distinct groups < cap=10 → success.
        let ok = run_aggregate(&path, &["g".into()], &aggs, 100, 10, 1, 100).expect("under cap");
        assert_eq!(ok.total_groups, 5);
        // Page 1 of 100 returns all 5 group rows.
        assert_eq!(ok.rows.height(), 5);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_cache_returns_not_found_with_materialize_hint() {
        let tool = AggregateDatasetTool::new(StubLookup::new(Some(CacheRef {
            id: Uuid::nil(),
            slug: "sales".into(),
            cached: false,
            cache_path: None,
        })));
        let err = tool
            .call(json!({"slug": "sales", "group_by": ["region"], "agg": [{"col": "amount", "fn": "sum"}]}))
            .await
            .expect_err("not materialised");
        match err {
            ToolError::NotFound(m) => assert!(m.contains("materialize_dataset"), "got: {m}"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_dataset_returns_not_found() {
        let tool = AggregateDatasetTool::new(StubLookup::new(None));
        let err = tool
            .call(json!({"slug": "no", "group_by": ["g"], "agg": [{"col": "v", "fn": "sum"}]}))
            .await
            .expect_err("unknown");
        assert!(matches!(err, ToolError::NotFound(_)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unsupported_cache_scheme_leaks_only_scheme_and_redacted_path() {
        let tool = AggregateDatasetTool::new(StubLookup::new(Some(CacheRef {
            id: Uuid::nil(),
            slug: "sales".into(),
            cached: true,
            cache_path: Some("s3://secret-bucket/key.parquet?sig=AAA".into()),
        })));
        let err = tool
            .call(json!({"slug": "sales", "group_by": ["r"], "agg": [{"col": "v", "fn": "sum"}]}))
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
        let d = AggregateDatasetTool::new(StubLookup::new(None)).descriptor();
        assert_eq!(d.name, "aggregate_dataset");
        assert!(d.output_schema.is_some());
    }
}
