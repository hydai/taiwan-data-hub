//! `query_rows` MCP tool — runs a user SQL string against a dataset's
//! cached Parquet via Polars, gated by the [`crate::sql_guard`] AST
//! whitelist.
//!
//! Pipeline:
//!
//! ```text
//!   user SQL  ──► sql_guard::validate  ──► Polars SQLContext
//!                                            │
//!                                            └─► scan_parquet(cache_path)
//!                                                registered as `current_dataset`
//! ```
//!
//! Defense-in-depth (DESIGN.md §6 + §9 / #1.7 DoD):
//!
//! - AST whitelist before Polars sees the SQL.
//! - Table whitelist: SQL can reference only `current_dataset`.
//! - `LIMIT` clamped to [`sql_guard::DEFAULT_MAX_LIMIT`] (`10_000`).
//! - `tokio::time::timeout` returns a deadline error to the caller
//!   after [`QUERY_TIMEOUT`]; see the note below on the limitation
//!   this carries.
//!
//! **Timeout limitation**: `tokio::time::timeout` wrapping
//! `spawn_blocking` is a *caller-side* deadline only. Dropping the
//! `JoinHandle` doesn't preempt an OS thread, so the Polars query
//! keeps running on the blocking pool until it naturally completes;
//! the caller gets `Execution("query exceeded …")` but resources
//! aren't reclaimed. The combination of the AST whitelist plus a
//! 10k LIMIT bounds what work a query can be made to do in the first
//! place, but a true hard kill needs worker-process isolation
//! (DESIGN.md §6 calls this out as a long-term safety measure;
//! tracked separately).
//!
//! **Streaming + memory limit** are tracked in #1.7's follow-ups;
//! the Polars `new_streaming` feature is on so the engine streams
//! when it can, but enforcing a hard memory ceiling needs Polars
//! 0.53's `engine_affinity` plumbing which is out of scope for this
//! PR.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use polars::prelude::*;
use polars::sql::SQLContext;
use serde_json::{Map, Value, json};
use storage::{CacheRef, DatasetCacheLookup, DatasetKey};
use uuid::Uuid;

use crate::sql_guard::{self, ALLOWED_TABLE, DEFAULT_MAX_LIMIT};

/// MCP tool name. Stable identifier — clients pin to this string.
pub const TOOL_NAME: &str = "query_rows";

/// Per-call deadline. Five seconds matches DESIGN.md §6's "tokio
/// timeout(5s)". A runaway scan would otherwise tie up an executor
/// thread until the OOM killer steps in.
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Reads from any [`DatasetCacheLookup`]; production code plugs in a
/// `storage::Storage`, tests plug in an in-memory stub.
#[derive(Clone)]
pub struct QueryRowsTool {
    lookup: Arc<dyn DatasetCacheLookup>,
}

impl QueryRowsTool {
    pub fn new<L: DatasetCacheLookup>(lookup: L) -> Self {
        Self {
            lookup: Arc::new(lookup),
        }
    }

    pub fn from_arc(lookup: Arc<dyn DatasetCacheLookup>) -> Self {
        Self { lookup }
    }
}

impl std::fmt::Debug for QueryRowsTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryRowsTool").finish_non_exhaustive()
    }
}

#[async_trait]
impl ToolHandler for QueryRowsTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: format!(
                "Run a SQL query against a dataset's cached Parquet. SQL must reference \
                 only `{ALLOWED_TABLE}`; LIMIT is capped at {DEFAULT_MAX_LIMIT}. Specify the \
                 dataset by `id` (UUID) or `slug`; exactly one is required. Returns up to \
                 the LIMIT rows plus a `truncated` flag. If the dataset hasn't been \
                 materialised yet, the tool returns a NotFound-shaped error asking the caller \
                 to invoke `materialize_dataset` first."
            ),
            input_schema: input_schema(),
            output_schema: Some(output_schema()),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let req = Request::parse(&args)?;
        let validated = sql_guard::validate(&req.sql, DEFAULT_MAX_LIMIT)
            .map_err(|e| ToolError::InvalidArguments(format!("sql: {e}")))?;

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

        let parquet_path = parquet_path_for_query(&cache)?;
        let effective_limit = validated.effective_limit();
        let validated_sql = validated.as_str().to_owned();
        let exec =
            tokio::task::spawn_blocking(move || run_polars_query(&parquet_path, &validated_sql));

        let df = match tokio::time::timeout(QUERY_TIMEOUT, exec).await {
            Ok(Ok(Ok(df))) => df,
            Ok(Ok(Err(e))) => {
                // Polars errors typically include the cache file
                // path, schema details, and other internal context.
                // Log the full error server-side so operators can
                // diagnose, but return a sanitised message to the
                // caller — leaking the cache layout would inform
                // follow-up attacks on a multi-tenant deploy.
                tracing::warn!(
                    slug = %cache.slug,
                    polars_error = %e,
                    "query_rows polars execution failed",
                );
                return Err(ToolError::Execution(
                    "query execution failed — see server logs for details".to_owned(),
                ));
            }
            Ok(Err(join_err)) => {
                tracing::error!(
                    slug = %cache.slug,
                    join_error = %join_err,
                    "query_rows worker join failed",
                );
                return Err(ToolError::Execution(
                    "query worker crashed unexpectedly".to_owned(),
                ));
            }
            Err(_) => {
                return Err(ToolError::Execution(format!(
                    "query exceeded {}s deadline",
                    QUERY_TIMEOUT.as_secs(),
                )));
            }
        };

        Ok(render_dataframe(&df, effective_limit))
    }
}

/// Resolve the file-system path Polars should scan, given a cache
/// reference. We only support `file://` and bare-path URIs for now;
/// `s3://` lands when #1.8 materialises to `SeaweedFS`.
fn parquet_path_for_query(cache: &CacheRef) -> Result<PathBuf, ToolError> {
    let (true, Some(raw)) = (cache.cached, cache.cache_path.as_deref()) else {
        return Err(ToolError::NotFound(format!(
            "dataset `{}` is not materialised yet — call `materialize_dataset` first",
            cache.slug,
        )));
    };

    if let Some(stripped) = raw.strip_prefix("file://") {
        Ok(PathBuf::from(stripped))
    } else if let Some(scheme) = extract_uri_scheme(raw) {
        // s3://, https://, etc. — not yet supported. Echo only the
        // scheme back to the caller; the full URI may carry bucket
        // names, internal hostnames, or (post-#1.8) signed-URL query
        // params we don't want leaking out of the server.
        tracing::warn!(
            slug = %cache.slug,
            cache_path = %raw,
            "cache uri scheme not yet supported by query_rows",
        );
        Err(ToolError::Execution(format!(
            "cache scheme `{scheme}` is not yet supported by query_rows"
        )))
    } else {
        Ok(PathBuf::from(raw))
    }
}

/// Pull the `scheme` part out of a `scheme://...` URI. Returns a
/// borrow tied to the input's lifetime — callers that need to keep
/// the scheme around past the input's drop should `.to_owned()`.
/// Returns `None` if the input doesn't look like a URI (no `://`).
fn extract_uri_scheme(uri: &str) -> Option<&str> {
    uri.split_once("://").map(|(scheme, _)| scheme)
}

/// Blocking helper that runs on `spawn_blocking`. Returns the
/// `DataFrame` so the async path can serialise it to JSON.
fn run_polars_query(path: &Path, sql: &str) -> PolarsResult<DataFrame> {
    // `PlRefPath` only implements `From<&str>`; lossy conversion is
    // fine for the file:// path use case (we already rejected anything
    // with a scheme upstream).
    let path_str = path.to_string_lossy();
    let lazy = LazyFrame::scan_parquet(path_str.as_ref().into(), ScanArgsParquet::default())?;
    let mut ctx = SQLContext::new();
    ctx.register(ALLOWED_TABLE, lazy);
    ctx.execute(sql)?.collect()
}

/// Convert a Polars `DataFrame` to the MCP JSON shape:
///
/// ```json
/// { "columns": ["a", "b"], "rows": [[1, "x"], …], "truncated": false }
/// ```
///
/// `truncated` is `true` iff the row count equals `effective_limit` —
/// the implementation can't distinguish "LIMIT held more rows back"
/// from "source data had exactly `effective_limit` rows", so the
/// flag is a conservative *may* signal: "you might have more rows
/// upstream; raise LIMIT to confirm." A future iteration could scan
/// `effective_limit + 1` rows à la the `search_datasets` cursor
/// trick to make this exact, at the cost of one extra row per
/// query.
fn render_dataframe(df: &DataFrame, effective_limit: u64) -> Value {
    let columns: Vec<Value> = df
        .get_column_names()
        .iter()
        .map(|c| Value::String(c.to_string()))
        .collect();

    // Hoist `df.columns()` and `df.width()` out of the row loop —
    // `columns()` returns a `&[Column]` slice in 0.53 but calling it
    // per row is still avoidable indirection that scales linearly
    // with row count.
    let columns_slice = df.columns();
    let width = columns_slice.len();
    let height = df.height();
    let mut rows: Vec<Value> = Vec::with_capacity(height);
    for row_idx in 0..height {
        let mut row = Vec::with_capacity(width);
        for column in columns_slice {
            row.push(any_value_to_json(
                &column.get(row_idx).unwrap_or(AnyValue::Null),
            ));
        }
        rows.push(Value::Array(row));
    }

    // The LIMIT clamp guarantees `height <= effective_limit` for any
    // non-degenerate query. Use `==` (not `>=`) so the contract reads
    // "the LIMIT held back more rows" instead of an off-by-one that
    // also fires on `LIMIT 0` (0 == 0 is the trivial empty case; the
    // user asked for nothing and got nothing, not a truncation).
    let truncated = effective_limit > 0 && (height as u64) == effective_limit;
    json!({
        "columns": columns,
        "rows": rows,
        "truncated": truncated,
    })
}

/// Best-effort conversion from a Polars cell to JSON. Anything we
/// don't have a dedicated mapping for falls back to its `Display`
/// representation as a string — preserves information without
/// pretending we support a richer type than we do, and `Display` is
/// the more user-friendly default for Polars `AnyValue` variants
/// (timestamps render in ISO-8601, lists as `[…]`, etc.).
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
        // NaN / Inf can't round-trip through JSON; surface them as
        // their textual form rather than mangling to null.
        AnyValue::Float32(n) => Value::String(format!("{n}")),
        AnyValue::Float64(n) => Value::String(format!("{n}")),
        other => Value::String(format!("{other}")),
    }
}

/// Parsed `tools/call` arguments. Mirrors `get_dataset`'s lookup
/// shape (id XOR slug) plus the `sql` string.
struct Request {
    key: DatasetKey,
    /// Borrow-friendly description of the key for error messages.
    lookup_repr: String,
    sql: String,
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

        let sql = match obj.get("sql") {
            None | Some(Value::Null) => {
                return Err(ToolError::InvalidArguments("`sql` is required".into()));
            }
            Some(Value::String(s)) if s.trim().is_empty() => {
                return Err(ToolError::InvalidArguments(
                    "`sql` must be non-empty".into(),
                ));
            }
            Some(Value::String(s)) => s.clone(),
            Some(other) => {
                return Err(ToolError::InvalidArguments(format!(
                    "`sql` must be a string, got {}",
                    kind_of(other)
                )));
            }
        };

        Ok(Self {
            key,
            lookup_repr,
            sql,
        })
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
    let mut props = Map::new();
    props.insert(
        "id".into(),
        json!({
            "type": "string",
            "format": "uuid",
            "description": "Dataset UUID. Exactly one of `id` or `slug` is required.",
        }),
    );
    props.insert(
        "slug".into(),
        json!({
            "type": "string",
            "description": "Marketplace slug. Exactly one of `id` or `slug` is required.",
        }),
    );
    props.insert(
        "sql".into(),
        json!({
            "type": "string",
            "minLength": 1,
            "description": format!(
                "SELECT statement against `{ALLOWED_TABLE}` (the virtual table this tool binds \
                 to the dataset's cached Parquet). LIMIT is capped at {DEFAULT_MAX_LIMIT}; \
                 functions, JOINs, CTEs, and subqueries are restricted — see the AST whitelist."
            ),
        }),
    );
    let mut schema = Map::new();
    schema.insert("type".into(), Value::String("object".into()));
    schema.insert(
        "required".into(),
        Value::Array(vec![Value::String("sql".into())]),
    );
    schema.insert("additionalProperties".into(), Value::Bool(false));
    schema.insert("properties".into(), Value::Object(props));
    schema
}

fn output_schema() -> Map<String, Value> {
    let mut schema = Map::new();
    schema.insert("type".into(), Value::String("object".into()));
    schema.insert(
        "required".into(),
        Value::Array(
            ["columns", "rows", "truncated"]
                .iter()
                .map(|s| Value::String((*s).to_string()))
                .collect(),
        ),
    );
    schema.insert(
        "properties".into(),
        json!({
            "columns": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Column names in the order matching each row.",
            },
            "rows": {
                "type": "array",
                "items": {
                    "type": "array",
                    "description": "One row, cell values in `columns` order.",
                },
            },
            "truncated": {
                "type": "boolean",
                "description":
                    "True if the result row count equals the LIMIT that ran (whichever was \
                     smaller: the LIMIT in the user's SQL, or the tool cap). A conservative \
                     `may` signal — there *could* be more rows the LIMIT held back, or the \
                     source could simply have had exactly that many rows. Raise the LIMIT \
                     to confirm.",
            },
        }),
    );
    schema
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use mcp_core::ToolError;
    use polars::prelude::{ParquetWriter, df};
    use storage::StorageError;
    use tempfile::TempDir;

    /// In-memory `DatasetCacheLookup`. Returns the same `CacheRef`
    /// for every key so we can drive the tool through scan + execute
    /// without a database.
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

    /// Write a tiny Parquet to a temp file and return its path plus
    /// the [`TempDir`] guard (caller must keep the guard alive).
    fn write_fixture_parquet() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("fixture.parquet");
        let mut df = df! {
            "id" => &[1_i64, 2, 3],
            "name" => &["alice", "bob", "carol"],
            "score" => &[10.5_f64, 12.0, 7.25],
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
    async fn happy_path_returns_columns_rows_and_truncated_flag() {
        let (_guard, path) = write_fixture_parquet();
        let lookup = StubLookup::new(Some(cache_ref_for(&path)));
        let tool = QueryRowsTool::new(lookup);

        let out = tool
            .call(json!({
                "slug": "fixture",
                "sql": "SELECT id, name FROM current_dataset ORDER BY id",
            }))
            .await
            .expect("query ok");

        let columns: Vec<&str> = out["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(columns, vec!["id", "name"]);

        let rows = out["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0][1], "alice");

        assert_eq!(out["truncated"], false);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_cache_returns_not_found_with_materialize_hint() {
        let lookup = StubLookup::new(Some(CacheRef {
            id: Uuid::nil(),
            slug: "fixture".into(),
            cached: false,
            cache_path: None,
        }));
        let tool = QueryRowsTool::new(lookup);

        let err = tool
            .call(json!({
                "slug": "fixture",
                "sql": "SELECT id FROM current_dataset",
            }))
            .await
            .unwrap_err();
        match err {
            ToolError::NotFound(m) => {
                assert!(m.contains("materialize_dataset"), "got: {m}");
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_dataset_returns_not_found() {
        let lookup = StubLookup::new(None);
        let tool = QueryRowsTool::new(lookup);

        let err = tool
            .call(json!({"slug": "no-such-thing", "sql": "SELECT 1 FROM current_dataset"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::NotFound(_)));
    }

    /// Polars surfaces internal context (file paths, schema, byte
    /// offsets, ...) in its error messages. The tool sanitises those
    /// before returning to the caller so a multi-tenant deploy
    /// doesn't leak cache layout. Trigger a Polars-level error
    /// (querying a column the file doesn't have) and assert the
    /// public message doesn't include the cache path or column name.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn polars_execution_errors_are_sanitised() {
        let (_guard, path) = write_fixture_parquet();
        let lookup = StubLookup::new(Some(cache_ref_for(&path)));
        let tool = QueryRowsTool::new(lookup);

        let err = tool
            .call(json!({
                "slug": "fixture",
                "sql": "SELECT nonexistent_column FROM current_dataset",
            }))
            .await
            .unwrap_err();
        match err {
            ToolError::Execution(m) => {
                assert!(
                    !m.contains(&path.to_string_lossy().to_string()),
                    "public error must not echo cache path: {m}",
                );
                assert!(
                    !m.contains("nonexistent_column"),
                    "public error must not echo column names: {m}",
                );
                assert!(
                    m.contains("server logs"),
                    "public error should point operators at the server logs: {m}",
                );
            }
            other => panic!("expected Execution, got {other:?}"),
        }
    }

    /// Unsupported cache scheme should surface only the scheme, not
    /// the full URI (which may carry bucket / hostname / signed-URL
    /// query params once #1.8 lands).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unsupported_cache_scheme_leaks_only_scheme() {
        let lookup = StubLookup::new(Some(CacheRef {
            id: Uuid::nil(),
            slug: "fixture".into(),
            cached: true,
            cache_path: Some(
                "s3://secret-bucket-internal/path/to/cache.parquet?signature=AAA".into(),
            ),
        }));
        let tool = QueryRowsTool::new(lookup);

        let err = tool
            .call(json!({"slug": "fixture", "sql": "SELECT a FROM current_dataset"}))
            .await
            .unwrap_err();
        match err {
            ToolError::Execution(m) => {
                assert!(m.contains("s3"), "scheme name should appear: {m}");
                assert!(
                    !m.contains("secret-bucket-internal"),
                    "bucket name must not leak: {m}",
                );
                assert!(
                    !m.contains("signature"),
                    "signed-URL params must not leak: {m}",
                );
            }
            other => panic!("expected Execution, got {other:?}"),
        }
    }

    #[test]
    fn extract_uri_scheme_only_when_separator_present() {
        assert_eq!(extract_uri_scheme("file:///tmp/x"), Some("file"));
        assert_eq!(extract_uri_scheme("s3://bucket/key"), Some("s3"));
        assert_eq!(extract_uri_scheme("https://example.com/x"), Some("https"));
        assert_eq!(extract_uri_scheme("/no/scheme/here"), None);
        assert_eq!(extract_uri_scheme(""), None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bad_sql_returns_invalid_arguments() {
        let (_guard, path) = write_fixture_parquet();
        let lookup = StubLookup::new(Some(cache_ref_for(&path)));
        let tool = QueryRowsTool::new(lookup);

        let err = tool
            .call(json!({
                "slug": "fixture",
                "sql": "DROP TABLE current_dataset",
            }))
            .await
            .unwrap_err();
        match err {
            ToolError::InvalidArguments(m) => assert!(m.contains("sql"), "got: {m}"),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_table_in_sql_rejected() {
        let (_guard, path) = write_fixture_parquet();
        let lookup = StubLookup::new(Some(cache_ref_for(&path)));
        let tool = QueryRowsTool::new(lookup);

        let err = tool
            .call(json!({
                "slug": "fixture",
                "sql": "SELECT 1 FROM pg_tables",
            }))
            .await
            .unwrap_err();
        match err {
            ToolError::InvalidArguments(m) => {
                assert!(m.contains("current_dataset"), "got: {m}");
            }
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_sql_field_rejected() {
        let lookup = StubLookup::new(Some(CacheRef {
            id: Uuid::nil(),
            slug: "fixture".into(),
            cached: true,
            cache_path: Some("/nonexistent".into()),
        }));
        let tool = QueryRowsTool::new(lookup);

        let err = tool.call(json!({"slug": "fixture"})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn both_id_and_slug_rejected() {
        let lookup = StubLookup::new(None);
        let tool = QueryRowsTool::new(lookup);

        let err = tool
            .call(json!({
                "id": Uuid::nil().to_string(),
                "slug": "fixture",
                "sql": "SELECT 1 FROM current_dataset",
            }))
            .await
            .unwrap_err();
        match err {
            ToolError::InvalidArguments(m) => {
                assert!(m.contains("only one"), "got: {m}");
            }
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn truncated_true_when_limit_holds_back_more_rows() {
        // Fixture has 3 rows; user-supplied `LIMIT 1` clamps to 1
        // (effective_limit = 1). Query yields 1 row, which equals
        // effective_limit ⇒ truncated=true. Operators read this as
        // "there's more upstream — raise LIMIT to see it."
        let (_guard, path) = write_fixture_parquet();
        let lookup = StubLookup::new(Some(cache_ref_for(&path)));
        let tool = QueryRowsTool::new(lookup);

        let out = tool
            .call(json!({
                "slug": "fixture",
                "sql": "SELECT id FROM current_dataset LIMIT 1",
            }))
            .await
            .expect("ok");
        assert_eq!(out["truncated"], true);
    }

    /// `LIMIT 0` is a valid SQL idiom for "tell me the columns
    /// without any rows". 0 rows == `effective_limit` 0, but we
    /// don't want to flag truncation in this degenerate case — the
    /// user got exactly what they asked for.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn truncated_false_when_limit_zero_returns_empty() {
        let (_guard, path) = write_fixture_parquet();
        let lookup = StubLookup::new(Some(cache_ref_for(&path)));
        let tool = QueryRowsTool::new(lookup);

        let out = tool
            .call(json!({
                "slug": "fixture",
                "sql": "SELECT id FROM current_dataset LIMIT 0",
            }))
            .await
            .expect("ok");
        assert_eq!(out["rows"].as_array().unwrap().len(), 0);
        assert_eq!(
            out["truncated"], false,
            "LIMIT 0 with 0 rows is not truncation"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn truncated_false_when_data_exhausted_within_limit() {
        // 3-row fixture, LIMIT 100 → returns 3 rows < effective_limit
        // (100). truncated=false because the data ran out, not the
        // cap. This is the user's "you have everything" signal.
        let (_guard, path) = write_fixture_parquet();
        let lookup = StubLookup::new(Some(cache_ref_for(&path)));
        let tool = QueryRowsTool::new(lookup);

        let out = tool
            .call(json!({
                "slug": "fixture",
                "sql": "SELECT id FROM current_dataset LIMIT 100",
            }))
            .await
            .expect("ok");
        assert_eq!(out["truncated"], false);
        assert_eq!(out["rows"].as_array().unwrap().len(), 3);
    }
}
