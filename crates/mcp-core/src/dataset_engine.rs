//! `DatasetEngine` — Polars `LazyFrame` helper for the rich MCP tools.
//!
//! All M3 rich tools (#3.2 `describe_schema`, #3.3 `get_sample`,
//! #3.4 `join_datasets`, #3.5 `aggregate_dataset`) build on this
//! surface. Each tool calls [`DatasetEngine::scan`] to get a
//! [`LazyFrame`], adds its own pipeline (filter, `group_by`, join, ...),
//! and finishes by either inspecting the lazy plan's schema or
//! collecting to a [`DataFrame`] via [`DatasetEngine::collect`].
//!
//! ## Threading model
//!
//! Polars is sync/blocking. Callers running under an async runtime
//! must wrap `collect()` in `tokio::task::spawn_blocking` (see
//! `tools-data::query_rows` for the established pattern). The engine
//! itself stays sync to keep the API the same for blocking and async
//! consumers.
//!
//! ## Memory bound
//!
//! Memory is bounded by two scan-time pushdowns Polars performs on
//! the returned `LazyFrame`:
//!
//! - [`LoadOptions::projection`] — column subset, pushed into the
//!   Parquet/CSV/JSON reader so only the requested columns are
//!   decoded.
//! - [`LoadOptions::row_limit`] — `.limit(n)` applied immediately
//!   after the scan; for Parquet this triggers row-group skipping.
//!
//! A *hard* memory ceiling needs Polars 0.53's `engine_affinity`
//! plumbing, which is out of scope for #3.1; the `new_streaming`
//! feature is already on so single-pass aggregations stream when
//! they can. Tools that want a smaller cap pass a smaller `row_limit`
//! — the engine doesn't impose its own ceiling because different
//! tools have different DoD-mandated caps (e.g. `query_rows` caps at
//! `10_000`, `get_sample` will cap at a much smaller default).
//!
//! ## Why `DatasetSource` is an enum, not extension sniffing
//!
//! Sniffing by `.ext` invites mismatched-format bugs the moment a
//! dataset is materialised under an unexpected name (the storage
//! layer is free to use UUID-based filenames). Callers know the
//! format at lookup time; making them say so explicitly removes a
//! footgun.
//!
//! ## Error sanitisation
//!
//! [`EngineError`] variants embed Polars' raw message, which can
//! include file paths and schema details. Callers that surface
//! errors to MCP clients should log the full error server-side and
//! return a sanitised message (the `query_rows` tool is the
//! reference implementation of this pattern).

use std::path::Path;

use polars::prelude::*;

/// Source format the engine can load. Each variant borrows the path
/// for the duration of the `scan` call — Polars copies it into the
/// lazy plan, so the borrow doesn't need to outlive the call.
#[derive(Debug, Clone, Copy)]
pub enum DatasetSource<'a> {
    /// Apache Parquet file. The expected on-disk format for cached
    /// datasets (see `tools-data::materialize_dataset`).
    Parquet(&'a Path),
    /// CSV file with a header row. The engine relies on Polars'
    /// default schema inference; callers needing typed columns must
    /// add an explicit `cast` after `scan`.
    Csv(&'a Path),
    /// Newline-delimited JSON (one record per line). Plain JSON
    /// arrays / single objects are not a supported *lazy* source
    /// upstream — Polars' `LazyJsonLineReader` only handles NDJSON.
    NdJson(&'a Path),
}

/// Constraints applied at scan time. Both fields are pushdown hints:
/// Polars folds them into the file reader where possible (column
/// pruning for Parquet, early `LIMIT` for all formats).
#[derive(Debug, Default, Clone)]
pub struct LoadOptions {
    /// Subset of column names to keep. `None` ⇒ all columns. Names
    /// that don't exist in the source surface as a Polars schema
    /// error at `collect()` — the engine doesn't pre-validate against
    /// the file's schema because doing so would require an extra scan
    /// that defeats the lazy contract.
    pub projection: Option<Vec<String>>,
    /// Hard cap on rows returned. `None` ⇒ no engine-level limit;
    /// the caller is responsible for bounding memory in that case.
    /// Tools typically pass a finite limit derived from their own
    /// Definition of Done (e.g. `10_000` for `query_rows`).
    pub row_limit: Option<u32>,
}

/// Errors the engine produces. Variants embed Polars' raw error text;
/// callers serialising to MCP clients should sanitise.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// Polars reader / scan / lazy-plan failure. The string is
    /// Polars' own `Display` of the underlying error and may include
    /// paths, schemas, byte offsets, or column names.
    #[error("polars: {0}")]
    Polars(String),
    /// Caller supplied an input the engine can't honour. Today the
    /// only producer is `scan` rejecting a non-UTF-8 dataset path —
    /// Polars' lazy readers take `&str`, and silently lossy
    /// conversion would mangle the path into a confusing
    /// "file not found" downstream. Future option validations
    /// (e.g. min/max bounds on `row_limit`) would surface here too.
    #[error("invalid input: {0}")]
    InvalidOption(String),
}

impl From<PolarsError> for EngineError {
    fn from(value: PolarsError) -> Self {
        Self::Polars(value.to_string())
    }
}

/// Namespace for the engine's associated functions. The struct
/// carries no state; methods are associated so the call sites read
/// `DatasetEngine::scan(...)` and consumers don't need to import
/// loose functions.
#[derive(Debug, Clone, Copy)]
pub struct DatasetEngine;

impl DatasetEngine {
    /// Scan a dataset into a [`LazyFrame`], applying projection and
    /// row-limit pushdowns from `opts`. Caller adds further pipeline
    /// steps (filter, `group_by`, join, ...) and collects on a
    /// blocking executor.
    pub fn scan(source: DatasetSource<'_>, opts: &LoadOptions) -> Result<LazyFrame, EngineError> {
        // Polars 0.53's lazy readers take a path type that converts
        // from `&str` via `Into`. We require the input `&Path` to be
        // valid UTF-8 rather than going through `to_string_lossy()`:
        // lossy conversion would silently mangle non-UTF-8 paths
        // (Latin-1 file names, raw bytes from a misconfigured FS) into
        // a confusing downstream "file not found", and the engine
        // can't tell the difference between "I lost a byte" and "this
        // file genuinely doesn't exist". Surfacing `InvalidOption`
        // here gives the caller a deterministic error to handle.
        let raw = match source {
            DatasetSource::Parquet(p) => {
                let s = utf8_path(p)?;
                LazyFrame::scan_parquet(s.into(), ScanArgsParquet::default())?
            }
            DatasetSource::Csv(p) => {
                let s = utf8_path(p)?;
                LazyCsvReader::new(s.into()).finish()?
            }
            DatasetSource::NdJson(p) => {
                let s = utf8_path(p)?;
                LazyJsonLineReader::new(s.into()).finish()?
            }
        };

        // Apply projection first so Polars pushes column selection
        // down into the file reader. Order matters: `.select` after
        // `.limit` would still work but Polars' optimiser can do less
        // with it.
        let projected = if let Some(cols) = &opts.projection {
            let exprs: Vec<Expr> = cols.iter().map(|c| col(c.as_str())).collect();
            raw.select(exprs)
        } else {
            raw
        };

        let limited = if let Some(n) = opts.row_limit {
            projected.limit(n)
        } else {
            projected
        };

        Ok(limited)
    }

    /// Collect a `LazyFrame` to a `DataFrame`. Convenience wrapper
    /// that preserves the engine's error type so callers don't have
    /// to import `polars::prelude::PolarsError` just to map it.
    ///
    /// **Blocking**: do not call from an async task without
    /// `spawn_blocking`. Polars `collect` is fully synchronous.
    pub fn collect(lf: LazyFrame) -> Result<DataFrame, EngineError> {
        Ok(lf.collect()?)
    }
}

/// Require the input path to be valid UTF-8. Polars' lazy readers
/// accept `&str`; falling back to `to_string_lossy()` would silently
/// drop non-UTF-8 bytes and leave the caller chasing a
/// "file not found" that isn't actually about the file.
fn utf8_path(path: &Path) -> Result<&str, EngineError> {
    path.to_str().ok_or_else(|| {
        EngineError::InvalidOption(format!(
            "dataset path must be valid UTF-8: {}",
            path.display(),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Write a 5-row Parquet fixture and return the path + the
    /// `TempDir` guard (caller must keep the guard alive).
    fn write_parquet_fixture() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("fixture.parquet");
        let mut df = df! {
            "id" => &[1_i64, 2, 3, 4, 5],
            "name" => &["a", "b", "c", "d", "e"],
            "score" => &[10.0_f64, 20.0, 30.0, 40.0, 50.0],
        }
        .expect("build df");
        let file = fs::File::create(&path).expect("create");
        ParquetWriter::new(file).finish(&mut df).expect("write");
        (dir, path)
    }

    fn write_csv_fixture() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("fixture.csv");
        // CSV literal is preferred over Polars' CsvWriter here so the
        // fixture stays readable and doesn't pull in extra writer
        // features. Header row + 5 data rows.
        fs::write(
            &path,
            "id,name,score\n1,a,10.0\n2,b,20.0\n3,c,30.0\n4,d,40.0\n5,e,50.0\n",
        )
        .expect("write csv");
        (dir, path)
    }

    fn write_ndjson_fixture() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("fixture.ndjson");
        // One JSON object per line — the format `LazyJsonLineReader`
        // actually reads. A plain JSON array would NOT work and would
        // surface as a Polars parse error.
        let body = concat!(
            r#"{"id":1,"name":"a","score":10.0}"#,
            "\n",
            r#"{"id":2,"name":"b","score":20.0}"#,
            "\n",
            r#"{"id":3,"name":"c","score":30.0}"#,
            "\n",
            r#"{"id":4,"name":"d","score":40.0}"#,
            "\n",
            r#"{"id":5,"name":"e","score":50.0}"#,
            "\n",
        );
        fs::write(&path, body).expect("write ndjson");
        (dir, path)
    }

    /// Helper: scan + collect, return the resulting `DataFrame`.
    fn collect(source: DatasetSource<'_>, opts: &LoadOptions) -> DataFrame {
        let lf = DatasetEngine::scan(source, opts).expect("scan ok");
        DatasetEngine::collect(lf).expect("collect ok")
    }

    #[test]
    fn parquet_round_trips_full_table() {
        let (_dir, path) = write_parquet_fixture();
        let df = collect(DatasetSource::Parquet(&path), &LoadOptions::default());
        assert_eq!(df.height(), 5);
        assert_eq!(df.width(), 3);
        let names = df.get_column_names();
        let cols: Vec<&str> = names.iter().map(AsRef::as_ref).collect();
        assert_eq!(cols, vec!["id", "name", "score"]);
    }

    #[test]
    fn csv_round_trips_full_table() {
        let (_dir, path) = write_csv_fixture();
        let df = collect(DatasetSource::Csv(&path), &LoadOptions::default());
        assert_eq!(df.height(), 5);
        assert_eq!(df.width(), 3);
    }

    #[test]
    fn ndjson_round_trips_full_table() {
        let (_dir, path) = write_ndjson_fixture();
        let df = collect(DatasetSource::NdJson(&path), &LoadOptions::default());
        assert_eq!(df.height(), 5);
        assert_eq!(df.width(), 3);
    }

    #[test]
    fn projection_keeps_only_named_columns() {
        let (_dir, path) = write_parquet_fixture();
        let opts = LoadOptions {
            projection: Some(vec!["id".into(), "score".into()]),
            row_limit: None,
        };
        let df = collect(DatasetSource::Parquet(&path), &opts);
        let names = df.get_column_names();
        let cols: Vec<&str> = names.iter().map(AsRef::as_ref).collect();
        assert_eq!(cols, vec!["id", "score"]);
    }

    #[test]
    fn projection_of_unknown_column_surfaces_polars_error() {
        let (_dir, path) = write_parquet_fixture();
        let opts = LoadOptions {
            projection: Some(vec!["nope".into()]),
            row_limit: None,
        };
        // The scan returns lazily — the column-mismatch error fires
        // when the plan runs, not when projection is registered.
        // `LazyFrame: !Debug` rules out `expect`, so match the Ok
        // explicitly.
        let scan_result = DatasetEngine::scan(DatasetSource::Parquet(&path), &opts);
        let Ok(lf) = scan_result else {
            panic!("scan should defer the column-mismatch to collect()");
        };
        let Err(err) = DatasetEngine::collect(lf) else {
            panic!("collect should fail for unknown column");
        };
        let msg = format!("{err}");
        assert!(msg.starts_with("polars:"), "got: {msg}");
        // Polars' message must mention the missing column for the
        // caller to know what went wrong (after they sanitise it for
        // outward-facing logs).
        assert!(msg.contains("nope"), "got: {msg}");
    }

    #[test]
    fn row_limit_clamps_returned_rows() {
        let (_dir, path) = write_parquet_fixture();
        let opts = LoadOptions {
            projection: None,
            row_limit: Some(2),
        };
        let df = collect(DatasetSource::Parquet(&path), &opts);
        assert_eq!(df.height(), 2);
    }

    #[test]
    fn row_limit_zero_returns_empty_frame_with_schema() {
        // LIMIT 0 is a legitimate "give me the schema only" idiom —
        // identical to its semantics in `query_rows`. The engine
        // returns a 0-row frame with the column structure intact.
        let (_dir, path) = write_parquet_fixture();
        let opts = LoadOptions {
            projection: None,
            row_limit: Some(0),
        };
        let df = collect(DatasetSource::Parquet(&path), &opts);
        assert_eq!(df.height(), 0);
        assert_eq!(df.width(), 3, "schema must survive row_limit=0");
    }

    #[test]
    fn row_limit_above_data_size_returns_all_rows() {
        let (_dir, path) = write_parquet_fixture();
        let opts = LoadOptions {
            projection: None,
            row_limit: Some(10_000),
        };
        let df = collect(DatasetSource::Parquet(&path), &opts);
        assert_eq!(df.height(), 5, "row_limit is a cap, not a target");
    }

    /// Non-UTF-8 dataset paths surface as `InvalidOption`, not a
    /// confusing "file not found" propagated from Polars after a
    /// lossy conversion. Constructing an invalid-UTF-8 `Path` is
    /// platform-specific: on Unix we can use `OsStr::from_bytes` to
    /// inject a byte that isn't valid UTF-8; Windows has different
    /// rules so we gate the test to Unix-family platforms.
    #[cfg(unix)]
    #[test]
    fn non_utf8_path_returns_invalid_option() {
        use std::os::unix::ffi::OsStrExt;
        let bad: &std::ffi::OsStr = std::ffi::OsStr::from_bytes(b"/tmp/\xFFinvalid.parquet");
        let path = Path::new(bad);
        let scan = DatasetEngine::scan(DatasetSource::Parquet(path), &LoadOptions::default());
        match scan {
            Err(EngineError::InvalidOption(msg)) => {
                assert!(msg.contains("UTF-8"), "got: {msg}");
            }
            Err(other) => panic!("expected InvalidOption, got: {other}"),
            Ok(_) => panic!("expected scan to reject non-UTF-8 path"),
        }
    }

    #[test]
    fn nonexistent_file_surfaces_polars_error() {
        // `LazyFrame` doesn't implement `Debug` so the usual
        // `.expect_err` path doesn't compile; spell out the match.
        // Some scans validate the path eagerly (Parquet hits the
        // footer), so the error may fire here; others defer until
        // `collect()`. Try both paths and assert one of them yields
        // a `Polars` error mentioning the missing file.
        let path = PathBuf::from("/tmp/__nope_nonexistent_fixture__.parquet");
        let scan_result =
            DatasetEngine::scan(DatasetSource::Parquet(&path), &LoadOptions::default());
        let err = match scan_result {
            Err(e) => e,
            Ok(lf) => match DatasetEngine::collect(lf) {
                Err(e) => e,
                Ok(_) => panic!("expected scan/collect to fail for missing file"),
            },
        };
        let msg = format!("{err}");
        assert!(msg.starts_with("polars:"), "got: {msg}");
    }

    /// The engine's return value is a *real* `LazyFrame` — downstream
    /// rich tools should be able to pipeline arbitrary lazy ops onto
    /// it (filter, `group_by`, agg). This test stands in for #3.5's
    /// pipeline by chaining a filter onto the scan's output.
    #[test]
    fn returned_lazyframe_is_pipelinable_with_filter() {
        let (_dir, path) = write_parquet_fixture();
        let lf = DatasetEngine::scan(DatasetSource::Parquet(&path), &LoadOptions::default())
            .expect("scan ok");
        let filtered = lf.filter(col("score").gt(lit(25.0)));
        let df = DatasetEngine::collect(filtered).expect("collect ok");
        assert_eq!(df.height(), 3, "scores > 25 → 30, 40, 50");
    }

    /// Projection + `row_limit` + downstream filter compose correctly:
    /// first 4 rows projected to (id, score), then filter on score
    /// runs against the projected view.
    #[test]
    fn projection_row_limit_and_downstream_filter_compose() {
        let (_dir, path) = write_parquet_fixture();
        let opts = LoadOptions {
            projection: Some(vec!["id".into(), "score".into()]),
            row_limit: Some(4),
        };
        let lf = DatasetEngine::scan(DatasetSource::Parquet(&path), &opts).expect("scan ok");
        let filtered = lf.filter(col("score").gt(lit(25.0)));
        let df = DatasetEngine::collect(filtered).expect("collect ok");
        // Limited to first 4 rows {id 1..=4, score 10..=40}; filter
        // keeps score > 25 ⇒ rows with id 3 and 4.
        assert_eq!(df.height(), 2);
        assert_eq!(df.width(), 2);
    }
}
