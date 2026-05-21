//! `join_datasets` MCP tool — joins two cached datasets on a shared
//! key (single-column or multi-column) via Polars' lazy join, with a
//! 1M-row safety cap that callers can override with `force=true`.
//!
//! Per #3.4 Definition of Done:
//!  - Inner / left / right / outer joins.
//!  - Single- or multi-column key (`on` accepts `string | string[]`).
//!  - Returns total row count and paginated rows.
//!  - Pre-flight cap at `MAX_JOIN_ROWS` (1,000,000); without `force`
//!    the tool refuses to materialise more.
//!
//! The lazy plan is bounded by `MAX_JOIN_ROWS + 1` so a runaway
//! Cartesian product can't OOM the worker. If `force=true`, the cap
//! still applies — callers wanting larger joins should pre-aggregate
//! one side first.

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

pub const TOOL_NAME: &str = "join_datasets";

/// Cap on rows the join is allowed to materialise. The lazy plan
/// applies `limit(MAX_JOIN_ROWS + 1)` so we can detect overflow
/// without doing the full Cartesian product.
pub const MAX_JOIN_ROWS: u32 = 1_000_000;

/// Row cap per *side* before the join — bounds the lazy plan input
/// so an unbounded scan can't dominate the join budget. Matches
/// `describe_schema` / `get_sample` for consistency.
pub const MAX_SCAN_ROWS_PER_SIDE: u32 = 100_000;

/// Default pagination size for the response. Smaller than
/// `query_rows`' 10k cap because the join output has wider rows
/// (two datasets' columns combined) and the agent reads them by hand.
pub const DEFAULT_PAGE_SIZE: u32 = 100;

/// Cap on a single response page. Same reasoning as `query_rows`'
/// `DEFAULT_MAX_LIMIT` — anything beyond is harder for an agent to
/// reason about than a follow-up paginated call.
pub const MAX_PAGE_SIZE: u32 = 1_000;

/// Per-call deadline. Larger than `query_rows`'s 5s because a join
/// over two 100k-row sides can do significantly more work than a
/// single-table `SELECT`.
///
/// **Caller-side only**: `tokio::time::timeout` wrapping
/// `spawn_blocking` returns a deadline error to the caller after
/// `JOIN_TIMEOUT`, but dropping the `JoinHandle` doesn't preempt an
/// OS thread. The Polars work keeps running on the blocking pool
/// until it naturally completes; resources aren't reclaimed. The
/// `MAX_SCAN_ROWS_PER_SIDE` + `MAX_JOIN_ROWS` caps bound the
/// worst-case wallclock; a true hard kill needs worker-process
/// isolation (DESIGN.md §6 long-term, tracked separately — same
/// caveat as `query_rows`).
const JOIN_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JoinHow {
    Inner,
    Left,
    Right,
    Outer,
}

impl JoinHow {
    fn as_wire(self) -> &'static str {
        match self {
            Self::Inner => "inner",
            Self::Left => "left",
            Self::Right => "right",
            Self::Outer => "outer",
        }
    }

    fn from_wire(s: &str) -> Option<Self> {
        match s {
            "inner" => Some(Self::Inner),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "outer" => Some(Self::Outer),
            _ => None,
        }
    }

    fn to_args(self) -> JoinArgs {
        match self {
            Self::Inner => JoinArgs::new(JoinType::Inner),
            Self::Left => JoinArgs::new(JoinType::Left),
            Self::Right => JoinArgs::new(JoinType::Right),
            // Polars 0.53 renamed "outer" → JoinType::Full.
            Self::Outer => JoinArgs::new(JoinType::Full),
        }
    }
}

const ACCEPTED_HOWS: &[&str] = &["inner", "left", "right", "outer"];

#[derive(Clone)]
pub struct JoinDatasetsTool {
    lookup: Arc<dyn DatasetCacheLookup>,
}

impl JoinDatasetsTool {
    pub fn new<L: DatasetCacheLookup>(lookup: L) -> Self {
        Self {
            lookup: Arc::new(lookup),
        }
    }

    pub fn from_arc(lookup: Arc<dyn DatasetCacheLookup>) -> Self {
        Self { lookup }
    }
}

impl std::fmt::Debug for JoinDatasetsTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoinDatasetsTool").finish_non_exhaustive()
    }
}

#[async_trait]
impl ToolHandler for JoinDatasetsTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: format!(
                "Join two cached datasets on a shared key. `left` and `right` each \
                 take `id` (UUID) or `slug`; `on` is the key column name (or array \
                 of names for multi-column keys). Supports `how`: inner | left | \
                 right | outer (default inner). Refuses joins materialising more \
                 than {MAX_JOIN_ROWS} rows unless `force=true`. Each side is scanned \
                 up to {MAX_SCAN_ROWS_PER_SIDE} rows. Response is paginated: \
                 `page` (1-based, default 1), `page_size` (default {DEFAULT_PAGE_SIZE}, \
                 max {MAX_PAGE_SIZE})."
            ),
            input_schema: input_schema(),
            output_schema: Some(output_schema()),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let req = Request::parse(&args)?;

        // Two cache lookups — fail fast on either side. We don't
        // parallelise the two awaits because the cache is in-process
        // (Postgres pool); sequential keeps the error path obvious.
        let left_cache = lookup_dataset(&self.lookup, &req.left).await?;
        let right_cache = lookup_dataset(&self.lookup, &req.right).await?;

        let left_path = parquet_path_for_join(&left_cache, "left")?;
        let right_path = parquet_path_for_join(&right_cache, "right")?;
        let left_slug = left_cache.slug.clone();
        let right_slug = right_cache.slug.clone();
        let on = req.on.clone();
        let how = req.how;
        let force = req.force;
        let page = req.page;
        let page_size = req.page_size;

        let work = tokio::task::spawn_blocking(move || {
            run_join(
                &left_path,
                &right_path,
                &on,
                how,
                force,
                page,
                page_size,
                MAX_SCAN_ROWS_PER_SIDE,
                MAX_JOIN_ROWS,
            )
        });

        match tokio::time::timeout(JOIN_TIMEOUT, work).await {
            Ok(Ok(Ok(report))) => Ok(report.render()),
            Ok(Ok(Err(JoinError::BadArgument(msg)))) => Err(ToolError::InvalidArguments(msg)),
            Ok(Ok(Err(JoinError::TooLarge { estimate, cap }))) => {
                Err(ToolError::InvalidArguments(format!(
                    "join would materialise at least {estimate} rows (cap {cap}); \
                     pass `force=true` to confirm, or pre-aggregate one side first",
                )))
            }
            Ok(Ok(Err(e))) => {
                tracing::warn!(
                    left = %left_slug,
                    right = %right_slug,
                    join_error = %e,
                    "join_datasets failed",
                );
                Err(ToolError::Execution(
                    "join failed — see server logs for details".into(),
                ))
            }
            Ok(Err(join_err)) => {
                tracing::error!(
                    left = %left_slug,
                    right = %right_slug,
                    join_error = %join_err,
                    "join_datasets worker join failed",
                );
                Err(ToolError::Execution(
                    "join worker crashed unexpectedly".into(),
                ))
            }
            Err(_) => Err(ToolError::Execution(format!(
                "join exceeded {}s deadline",
                JOIN_TIMEOUT.as_secs(),
            ))),
        }
    }
}

#[derive(Debug, Error)]
enum JoinError {
    #[error("{0}")]
    Engine(#[from] EngineError),
    #[error("join_datasets: {0}")]
    BadArgument(String),
    #[error("join would materialise {estimate} rows (cap {cap})")]
    TooLarge { estimate: u64, cap: u32 },
}

#[allow(clippy::too_many_arguments)] // the worker is self-contained;
// fewer params at this junction would mean introducing a struct that
// only this one function uses.
fn run_join(
    left_path: &Path,
    right_path: &Path,
    on: &[String],
    how: JoinHow,
    force: bool,
    page: u32,
    page_size: u32,
    scan_cap: u32,
    join_cap: u32,
) -> Result<JoinReport, JoinError> {
    let left_lf = DatasetEngine::scan(
        DatasetSource::Parquet(left_path),
        &LoadOptions {
            projection: None,
            row_limit: Some(scan_cap),
        },
    )?;
    let right_lf = DatasetEngine::scan(
        DatasetSource::Parquet(right_path),
        &LoadOptions {
            projection: None,
            row_limit: Some(scan_cap),
        },
    )?;

    // Polars' join expressions take `Vec<Expr>`. Reuse the same key
    // list on both sides — the DoD allows multi-column keys but does
    // not require asymmetric `left_on / right_on`; supporting that
    // is a follow-up if a real use case emerges.
    let on_exprs: Vec<Expr> = on.iter().map(|c| col(c.as_str())).collect();

    let cap_plus_one = join_cap.saturating_add(1);
    let joined_lf = left_lf.join(right_lf, on_exprs.clone(), on_exprs, how.to_args());

    // Pass 1: count probe with `.limit(cap + 1)`. We never
    // materialise the full join into memory — `select([len()])`
    // collapses to one cell. The +1 lets us tell "exactly cap"
    // from "clipped".
    let probe_lf = joined_lf.clone().limit(cap_plus_one);
    let count_frame =
        DatasetEngine::collect(probe_lf.select([len()])).map_err(|e| classify(e, on))?;
    let total_probe = parse_probe_count(&count_frame)?;
    let exceeded = total_probe > u64::from(join_cap);

    if exceeded && !force {
        return Err(JoinError::TooLarge {
            estimate: u64::from(join_cap) + 1,
            cap: join_cap,
        });
    }

    // Effective total after the cap clamp. Even with force=true the
    // materialised total honours MAX_JOIN_ROWS — `force` exists to
    // confirm intent, not to disable the bounded-output contract.
    // total_probe is bounded by `join_cap + 1`, so try_from never
    // saturates on 64-bit; the unwrap is conservative defence.
    let usable_total = usize::try_from(total_probe.min(u64::from(join_cap))).unwrap_or(usize::MAX);

    // Pass 2: collect only the requested page. **Crucially** this
    // re-uses the *un-probed* `joined_lf` and applies a fresh
    // `.limit(join_cap)` (not cap+1) so the materialised page can't
    // include the probe's "is there one extra row?" overflow row.
    // If we re-used `probe_lf` here, a force=true caller could see
    // an extra row beyond the cap. `.slice(offset, page_size)` is
    // folded into the lazy plan, so Polars stops joining once
    // enough rows accumulate for the slice — page 1 of 100 does
    // ≈100 rows of work even when the full join is much larger.
    let offset = i64::from(page.saturating_sub(1)).saturating_mul(i64::from(page_size));
    let page_lf = joined_lf.limit(join_cap).slice(offset, page_size);
    let collected_page = DatasetEngine::collect(page_lf).map_err(|e| classify(e, on))?;

    Ok(JoinReport {
        how_wire: how.as_wire(),
        page,
        page_size,
        total_rows: usable_total,
        exceeded,
        rows: collected_page,
    })
}

/// Pull the row count out of the 1-cell aggregation frame Polars
/// returns from `select([len()])`. Every unexpected shape — missing
/// column, empty cell, unhandled dtype, negative integer — surfaces
/// as `JoinError::Engine` so a Polars contract drift can't silently
/// degrade to "0 rows" and bypass the over-cap guard.
fn parse_probe_count(frame: &DataFrame) -> Result<u64, JoinError> {
    let column = frame
        .column("len")
        .map_err(|_| EngineError::Polars("join probe missing `len` column".into()))?;
    let cell = column
        .get(0)
        .map_err(|_| EngineError::Polars("join probe count returned empty result".into()))?;
    match cell {
        AnyValue::UInt32(n) => Ok(u64::from(n)),
        AnyValue::UInt64(n) => Ok(n),
        AnyValue::Int64(n) => u64::try_from(n).map_err(|_| {
            EngineError::Polars(format!("join probe count returned negative Int64 ({n})")).into()
        }),
        AnyValue::Int32(n) => u64::try_from(n).map_err(|_| {
            EngineError::Polars(format!("join probe count returned negative Int32 ({n})")).into()
        }),
        other => Err(EngineError::Polars(format!(
            "join probe count returned unexpected type: {other}"
        ))
        .into()),
    }
}

/// Classify a Polars collect error: the most common surface-level
/// caller mistake is a missing join key column. Sniff for the
/// requested key names in the error wording (Polars uses
/// `ColumnNotFound` / `not found` / `unable to find`) and surface
/// the actionable case as `BadArgument` — but **without** echoing
/// the raw engine message, which may carry cache paths or schema
/// details. The full message still ends up server-side via the
/// `tracing::warn!` in `call()` because the `BadArgument` variant
/// itself implements `Display`.
fn classify(err: EngineError, on: &[String]) -> JoinError {
    let EngineError::Polars(ref msg) = err else {
        return JoinError::Engine(err);
    };
    let mentions_key = on.iter().any(|c| msg.contains(c.as_str()));
    let mentions_missing = msg.contains("not found")
        || msg.contains("ColumnNotFound")
        || msg.contains("unable to find");
    if mentions_key && mentions_missing {
        JoinError::BadArgument(format!("join key {on:?} not found in one or both datasets"))
    } else {
        JoinError::Engine(err)
    }
}

#[derive(Debug)]
struct JoinReport {
    how_wire: &'static str,
    page: u32,
    page_size: u32,
    /// Total rows in the materialised join (after the `MAX_JOIN_ROWS`
    /// cap was applied). `< total_height` when the cap clipped.
    total_rows: usize,
    /// True when the cap+1 probe surfaced an extra row, signalling
    /// the join was larger than the cap and `force=true` was used to
    /// continue anyway.
    exceeded: bool,
    rows: DataFrame,
}

impl JoinReport {
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
        // total_pages = ceil(total_rows / page_size), clamped to
        // ≥ 1 for the empty-result case (still has a "page 1").
        // Integer math avoids the float-cast lints + truncation
        // risk a (f64/f64).ceil() as u32 would carry.
        let page_size_usize = (self.page_size as usize).max(1);
        let total_pages_usize = self.total_rows.div_ceil(page_size_usize).max(1);
        let total_pages = u32::try_from(total_pages_usize).unwrap_or(u32::MAX);
        json!({
            "how": self.how_wire,
            "page": self.page,
            "page_size": self.page_size,
            "total_pages": total_pages,
            "total_rows": self.total_rows,
            "exceeded_cap": self.exceeded,
            "columns": columns,
            "rows": rows,
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

async fn lookup_dataset(
    lookup: &Arc<dyn DatasetCacheLookup>,
    side: &DatasetSide,
) -> Result<CacheRef, ToolError> {
    let cache = lookup
        .dataset_cache(side.key.clone())
        .await
        .map_err(|e| ToolError::Execution(format!("storage: {e}")))?;
    cache.ok_or_else(|| {
        ToolError::NotFound(format!(
            "{} dataset not found ({})",
            side.label, side.lookup_repr
        ))
    })
}

fn parquet_path_for_join(cache: &CacheRef, side: &str) -> Result<PathBuf, ToolError> {
    let (true, Some(raw)) = (cache.cached, cache.cache_path.as_deref()) else {
        return Err(ToolError::NotFound(format!(
            "{side} dataset `{}` is not materialised yet — call `materialize_dataset` first",
            cache.slug,
        )));
    };
    if let Some(stripped) = raw.strip_prefix("file://") {
        Ok(PathBuf::from(stripped))
    } else if let Some(scheme) = raw.split_once("://").map(|(s, _)| s) {
        tracing::warn!(
            slug = %cache.slug,
            cache_scheme = %scheme,
            "cache uri scheme not yet supported by join_datasets",
        );
        Err(ToolError::Execution(format!(
            "cache scheme `{scheme}` is not yet supported by join_datasets"
        )))
    } else {
        Ok(PathBuf::from(raw))
    }
}

struct DatasetSide {
    label: &'static str,
    key: DatasetKey,
    lookup_repr: String,
}

struct Request {
    left: DatasetSide,
    right: DatasetSide,
    on: Vec<String>,
    how: JoinHow,
    page: u32,
    page_size: u32,
    force: bool,
}

impl Request {
    fn parse(args: &Value) -> Result<Self, ToolError> {
        let obj = args
            .as_object()
            .ok_or_else(|| ToolError::InvalidArguments("arguments must be a JSON object".into()))?;

        let left_obj = require_object(obj, "left")?;
        let right_obj = require_object(obj, "right")?;
        let left = parse_side("left", left_obj)?;
        let right = parse_side("right", right_obj)?;

        let on = parse_on(obj)?;

        let how_wire = optional_string(obj, "how")?.unwrap_or_else(|| "inner".into());
        let how = JoinHow::from_wire(&how_wire).ok_or_else(|| {
            ToolError::InvalidArguments(format!(
                "`how` must be one of {ACCEPTED_HOWS:?}, got {how_wire:?}"
            ))
        })?;

        let page = parse_positive_u32(obj, "page", 1, u32::MAX)?;
        let page_size = parse_positive_u32(obj, "page_size", DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE)?;
        let force = match obj.get("force") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(other) => {
                return Err(ToolError::InvalidArguments(format!(
                    "`force` must be a boolean, got {}",
                    kind_of(other)
                )));
            }
        };

        Ok(Self {
            left,
            right,
            on,
            how,
            page,
            page_size,
            force,
        })
    }
}

fn require_object<'a>(
    obj: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a Map<String, Value>, ToolError> {
    let v = obj.get(key).ok_or_else(|| {
        ToolError::InvalidArguments(format!("`{key}` is required (object with id or slug)"))
    })?;
    v.as_object().ok_or_else(|| {
        ToolError::InvalidArguments(format!(
            "`{key}` must be an object with `id` or `slug`, got {}",
            kind_of(v)
        ))
    })
}

fn parse_side(label: &'static str, obj: &Map<String, Value>) -> Result<DatasetSide, ToolError> {
    let id = optional_string(obj, "id")?
        .map(|s| Uuid::parse_str(&s))
        .transpose()
        .map_err(|e| {
            ToolError::InvalidArguments(format!("`{label}.id` is not a valid UUID: {e}"))
        })?;
    let slug = optional_string(obj, "slug")?;
    match (id, slug) {
        (Some(id), None) => Ok(DatasetSide {
            label,
            key: DatasetKey::Id(id),
            lookup_repr: format!("id={id}"),
        }),
        (None, Some(slug)) => {
            let repr = format!("slug={slug}");
            Ok(DatasetSide {
                label,
                key: DatasetKey::Slug(slug),
                lookup_repr: repr,
            })
        }
        (None, None) => Err(ToolError::InvalidArguments(format!(
            "`{label}` must specify `id` or `slug`"
        ))),
        (Some(_), Some(_)) => Err(ToolError::InvalidArguments(format!(
            "`{label}` must specify only one of `id` or `slug`"
        ))),
    }
}

fn parse_on(obj: &Map<String, Value>) -> Result<Vec<String>, ToolError> {
    match obj.get("on") {
        None | Some(Value::Null) => Err(ToolError::InvalidArguments(
            "`on` is required (string or array of column names)".into(),
        )),
        Some(Value::String(s)) if s.is_empty() => Err(ToolError::InvalidArguments(
            "`on` must be a non-empty column name".into(),
        )),
        Some(Value::String(s)) => Ok(vec![s.clone()]),
        Some(Value::Array(arr)) => {
            if arr.is_empty() {
                return Err(ToolError::InvalidArguments(
                    "`on` must list at least one column name".into(),
                ));
            }
            let mut out = Vec::with_capacity(arr.len());
            for (idx, v) in arr.iter().enumerate() {
                match v {
                    Value::String(s) if !s.is_empty() => out.push(s.clone()),
                    Value::String(_) => {
                        return Err(ToolError::InvalidArguments(format!(
                            "`on[{idx}]` must be a non-empty column name"
                        )));
                    }
                    other => {
                        return Err(ToolError::InvalidArguments(format!(
                            "`on[{idx}]` must be a string, got {}",
                            kind_of(other)
                        )));
                    }
                }
            }
            Ok(out)
        }
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`on` must be a string or array, got {}",
            kind_of(other)
        ))),
    }
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
    let dataset_ref = json!({
        "type": "object",
        "properties": {
            "id": { "type": "string", "format": "uuid", "description": "Dataset UUID." },
            "slug": { "type": "string", "description": "Marketplace slug." },
        },
        "additionalProperties": false,
        "description": "Specify exactly one of `id` or `slug`.",
    });
    let how_description = format!(
        "Join type. Supported: {ACCEPTED_HOWS:?}. `outer` maps to Polars `JoinType::Full`."
    );
    let force_description =
        format!("Override the {MAX_JOIN_ROWS}-row cap when the join would otherwise be refused.");
    json!({
        "type": "object",
        "required": ["left", "right", "on"],
        "properties": {
            "left": dataset_ref.clone(),
            "right": dataset_ref,
            "on": {
                "oneOf": [
                    { "type": "string", "minLength": 1 },
                    { "type": "array", "items": { "type": "string", "minLength": 1 }, "minItems": 1 },
                ],
                "description": "Join key column name (string), or array of names for multi-column keys. Same column names are used on both sides.",
            },
            "how": {
                "type": "string",
                "enum": ACCEPTED_HOWS,
                "default": "inner",
                "description": how_description,
            },
            "page": {
                "type": "integer",
                "minimum": 1,
                "default": 1,
                "description": "1-based page number into the joined result.",
            },
            "page_size": {
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_PAGE_SIZE,
                "default": DEFAULT_PAGE_SIZE,
                "description": "Rows per page.",
            },
            "force": {
                "type": "boolean",
                "default": false,
                "description": force_description,
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
        "required": ["how", "page", "page_size", "total_pages", "total_rows", "exceeded_cap", "columns", "rows"],
        "properties": {
            "how": { "type": "string", "enum": ACCEPTED_HOWS },
            "page": { "type": "integer", "minimum": 1 },
            "page_size": { "type": "integer", "minimum": 1 },
            "total_pages": { "type": "integer", "minimum": 1 },
            "total_rows": { "type": "integer", "minimum": 0, "description": "Total rows in the materialised join (post-cap)." },
            "exceeded_cap": { "type": "boolean", "description": "True when the cap probe surfaced an extra row, i.e. the un-capped join would have produced more than MAX_JOIN_ROWS. Requires `force=true` to materialise." },
            "columns": { "type": "array", "items": { "type": "string" } },
            "rows": { "type": "array", "items": { "type": "array" } },
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

    /// Stub returns a fixed map of `slug` to `CacheRef`. We need different
    /// answers for `left` and `right` so a single `CacheRef` stub from
    /// the earlier tools doesn't work.
    #[derive(Clone, Default)]
    struct StubLookup {
        by_slug: Arc<Mutex<std::collections::HashMap<String, CacheRef>>>,
    }

    impl StubLookup {
        fn insert(&self, slug: &str, cache: CacheRef) {
            self.by_slug.lock().unwrap().insert(slug.to_string(), cache);
        }
    }

    #[async_trait]
    impl DatasetCacheLookup for StubLookup {
        async fn dataset_cache(&self, key: DatasetKey) -> Result<Option<CacheRef>, StorageError> {
            let slug = match key {
                DatasetKey::Slug(s) => s,
                DatasetKey::Id(_) => return Ok(None),
            };
            Ok(self.by_slug.lock().unwrap().get(&slug).cloned())
        }
    }

    fn write_users() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("users.parquet");
        let mut df = df! {
            "uid" => &[1_i64, 2, 3, 4],
            "name" => &["a", "b", "c", "d"],
        }
        .expect("build users");
        let file = std::fs::File::create(&path).expect("create");
        ParquetWriter::new(file).finish(&mut df).expect("write");
        (dir, path)
    }

    fn write_orders() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("orders.parquet");
        // user 4 has no orders → left join keeps them with null amount;
        // inner drops them. order 99 references unknown user → outer
        // surfaces.
        let mut df = df! {
            "uid" => &[1_i64, 2, 2, 3, 99],
            "amount" => &[10_f64, 20.0, 30.0, 40.0, 50.0],
        }
        .expect("build orders");
        let file = std::fs::File::create(&path).expect("create");
        ParquetWriter::new(file).finish(&mut df).expect("write");
        (dir, path)
    }

    fn cache_ref_for(path: &Path, slug: &str) -> CacheRef {
        CacheRef {
            id: Uuid::nil(),
            slug: slug.into(),
            cached: true,
            cache_path: Some(path.to_string_lossy().into_owned()),
        }
    }

    fn build_tool() -> (TempDir, TempDir, JoinDatasetsTool) {
        let (u_dir, u_path) = write_users();
        let (o_dir, o_path) = write_orders();
        let stub = StubLookup::default();
        stub.insert("users", cache_ref_for(&u_path, "users"));
        stub.insert("orders", cache_ref_for(&o_path, "orders"));
        (u_dir, o_dir, JoinDatasetsTool::new(stub))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn inner_join_returns_matching_rows() {
        let (_u, _o, tool) = build_tool();
        let out = tool
            .call(json!({
                "left": {"slug": "users"},
                "right": {"slug": "orders"},
                "on": "uid",
            }))
            .await
            .expect("ok");
        assert_eq!(out["how"], "inner");
        // uid=1 → 1 row; uid=2 → 2 rows; uid=3 → 1 row; uid=4 unmatched;
        // uid=99 unmatched. Total = 4.
        assert_eq!(out["total_rows"], 4);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn left_join_keeps_unmatched_left_rows() {
        let (_u, _o, tool) = build_tool();
        let out = tool
            .call(json!({
                "left": {"slug": "users"},
                "right": {"slug": "orders"},
                "on": "uid",
                "how": "left",
            }))
            .await
            .expect("ok");
        // 4 users, uid=4 unmatched but kept with null amount.
        // uid=2 has 2 orders → 1+2+1+1 = 5 rows total.
        assert_eq!(out["total_rows"], 5);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn right_join_keeps_unmatched_right_rows() {
        let (_u, _o, tool) = build_tool();
        let out = tool
            .call(json!({
                "left": {"slug": "users"},
                "right": {"slug": "orders"},
                "on": "uid",
                "how": "right",
            }))
            .await
            .expect("ok");
        // 4 matched + 1 unmatched order (uid=99 has no user) = 5.
        // uid=4 user is dropped (unmatched left side discarded).
        assert_eq!(out["how"], "right");
        assert_eq!(out["total_rows"], 5);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn outer_join_keeps_unmatched_both_sides() {
        let (_u, _o, tool) = build_tool();
        let out = tool
            .call(json!({
                "left": {"slug": "users"},
                "right": {"slug": "orders"},
                "on": "uid",
                "how": "outer",
            }))
            .await
            .expect("ok");
        // 4 matched + 1 unmatched user (uid=4) + 1 unmatched order
        // (uid=99) = 6. uid=2 contributes 2 matched rows.
        assert_eq!(out["total_rows"], 6);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pagination_returns_correct_slice() {
        let (_u, _o, tool) = build_tool();
        let out = tool
            .call(json!({
                "left": {"slug": "users"},
                "right": {"slug": "orders"},
                "on": "uid",
                "page_size": 2,
                "page": 2,
            }))
            .await
            .expect("ok");
        assert_eq!(out["page"], 2);
        assert_eq!(out["page_size"], 2);
        assert_eq!(out["total_pages"], 2);
        // 4 rows total, page 2 of size 2 → 2 rows.
        assert_eq!(out["rows"].as_array().unwrap().len(), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn page_out_of_range_returns_empty_rows() {
        let (_u, _o, tool) = build_tool();
        let out = tool
            .call(json!({
                "left": {"slug": "users"},
                "right": {"slug": "orders"},
                "on": "uid",
                "page_size": 100,
                "page": 5,
            }))
            .await
            .expect("ok");
        assert_eq!(out["rows"].as_array().unwrap().len(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_join_key_returns_invalid_arguments() {
        let (_u, _o, tool) = build_tool();
        let err = tool
            .call(json!({
                "left": {"slug": "users"},
                "right": {"slug": "orders"},
                "on": "nope",
            }))
            .await
            .expect_err("missing key");
        match err {
            ToolError::InvalidArguments(m) => assert!(m.contains("nope"), "got: {m}"),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_on_rejected() {
        let (_u, _o, tool) = build_tool();
        let err = tool
            .call(json!({
                "left": {"slug": "users"},
                "right": {"slug": "orders"},
            }))
            .await
            .expect_err("missing on");
        match err {
            ToolError::InvalidArguments(m) => assert!(m.contains("on"), "got: {m}"),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_how_rejected() {
        let (_u, _o, tool) = build_tool();
        let err = tool
            .call(json!({
                "left": {"slug": "users"},
                "right": {"slug": "orders"},
                "on": "uid",
                "how": "cross",
            }))
            .await
            .expect_err("unknown how");
        match err {
            ToolError::InvalidArguments(m) => assert!(m.contains("how"), "got: {m}"),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn left_dataset_not_found_yields_not_found() {
        let stub = StubLookup::default();
        // Only register "orders" — "users" missing.
        let (_o, o_path) = write_orders();
        stub.insert("orders", cache_ref_for(&o_path, "orders"));
        let tool = JoinDatasetsTool::new(stub);
        let err = tool
            .call(json!({
                "left": {"slug": "users"},
                "right": {"slug": "orders"},
                "on": "uid",
            }))
            .await
            .expect_err("left missing");
        match err {
            ToolError::NotFound(m) => {
                assert!(m.contains("left"), "got: {m}");
                assert!(m.contains("users"), "got: {m}");
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_left_side_rejected() {
        let tool = JoinDatasetsTool::new(StubLookup::default());
        let err = tool
            .call(json!({
                "right": {"slug": "orders"},
                "on": "uid",
            }))
            .await
            .expect_err("no left");
        match err {
            ToolError::InvalidArguments(m) => assert!(m.contains("left"), "got: {m}"),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    /// Locks the safety-cap contract:
    ///  - over-cap join with `force=false` → `JoinError::TooLarge`
    ///  - over-cap join with `force=true`  → materialises only up
    ///    to `join_cap` rows, sets `exceeded=true`
    ///  - under-cap join → `exceeded=false`, `total_rows=actual`
    ///
    /// Uses a tiny per-test `join_cap` so we don't have to build
    /// 1M-row fixtures. Plain `#[test]` (not `#[tokio::test]`)
    /// because `run_join` is synchronous; Polars `collect` spins up
    /// its own runtime and would panic in a Tokio context.
    #[test]
    fn safety_cap_rejects_without_force_and_truncates_with_force() {
        let (_u, u_path) = write_users();
        let (_o, o_path) = write_orders();
        let on = vec!["uid".to_string()];

        // Inner join produces 4 rows on the fixture. Setting join_cap=2
        // is over-cap → should reject when force=false.
        let err = run_join(&u_path, &o_path, &on, JoinHow::Inner, false, 1, 100, 100, 2)
            .expect_err("over-cap without force");
        match err {
            JoinError::TooLarge { cap, estimate } => {
                assert_eq!(cap, 2);
                assert!(estimate >= 3, "estimate is cap+1 = 3, got {estimate}");
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }

        // Same call with force=true → materialises only cap rows.
        let report =
            run_join(&u_path, &o_path, &on, JoinHow::Inner, true, 1, 100, 100, 2).expect("forced");
        assert_eq!(report.total_rows, 2, "total_rows clamped to cap");
        assert!(report.exceeded, "exceeded flag set when force=true clipped");

        // Under-cap join — actual total returned, exceeded=false.
        let under = run_join(
            &u_path,
            &o_path,
            &on,
            JoinHow::Inner,
            false,
            1,
            100,
            100,
            100,
        )
        .expect("under cap");
        assert_eq!(under.total_rows, 4);
        assert!(!under.exceeded);
    }

    #[test]
    fn descriptor_advertises_input_and_output_schemas() {
        let d = JoinDatasetsTool::new(StubLookup::default()).descriptor();
        assert_eq!(d.name, "join_datasets");
        assert!(d.output_schema.is_some());
    }
}
