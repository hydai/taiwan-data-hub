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
//! Polars is sync/blocking, and both halves of the pipeline can do
//! real I/O:
//!
//! - [`DatasetEngine::scan`] *may* do blocking metadata or
//!   schema-inference I/O depending on the format and Polars
//!   version (CSV / NDJSON typically probe rows to infer types;
//!   Parquet's `scan_parquet` is mostly lazy today but may
//!   validate the footer eagerly in future releases). Treat it
//!   as potentially blocking.
//! - [`DatasetEngine::collect`] executes the lazy plan and is
//!   always blocking — this is where the file body actually
//!   gets read.
//!
//! Callers running under an async runtime should wrap the **whole**
//! `scan → pipeline → collect` chain in `tokio::task::spawn_blocking`,
//! not just the `collect()`. The engine itself stays sync to keep
//! the API identical for blocking and async consumers; see
//! `tools_data::query_rows` for the established pattern.
//!
//! ## Memory bound
//!
//! `LoadOptions` bounds *scan-time* memory only — what's read from
//! the source and decoded into the lazy plan's first stage:
//!
//! - [`LoadOptions::projection`] — column subset, pushed into the
//!   Parquet/CSV/JSON reader so only the requested columns are
//!   decoded.
//! - [`LoadOptions::row_limit`] — `.limit(n)` applied immediately
//!   after the scan; for Parquet this triggers row-group skipping.
//!
//! These are **not** a general memory ceiling: downstream lazy ops
//! the caller adds (joins, sorts, wide `group_by`s, `explode`,
//! window functions) can materialise far more rows or memory than
//! the scan let in. Callers needing a hard ceiling at the *final*
//! result level have to bound their own pipeline (e.g. `.limit(n)`
//! after the heavy op, or pre-aggregating).
//!
//! A *true* hard memory ceiling at the engine level needs Polars
//! 0.53's `engine_affinity` plumbing, which is out of scope for
//! #3.1; the `new_streaming` feature is already on so single-pass
//! aggregations stream when they can. Tools that want a smaller
//! scan cap pass a smaller `row_limit` — the engine doesn't impose
//! its own ceiling because different tools have different
//! DoD-mandated caps (e.g. `query_rows` caps at `10_000`,
//! `get_sample` will cap at a much smaller default).
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
//! **Treat every [`EngineError`] variant as potentially sensitive.**
//! - [`EngineError::Polars`] is built as
//!   `<op>[ (<path>)]: <polars message>` — the leading op label
//!   (`scan parquet`, `scan csv`, `scan ndjson`, or `collect`) and
//!   the optional path are *engine-controlled* and form a stable
//!   contract callers/tests can assert on; the trailing message is
//!   Polars' raw `Display` and can include schema details, byte
//!   offsets, and column names (and isn't a stable assertion
//!   target — Polars wording shifts across patch releases).
//! - [`EngineError::InvalidOption`] includes a lossy-rendered form
//!   of the offending input — today that's the dataset path via
//!   `path.display()`. The exact byte sequence isn't preserved (and
//!   *can't* be when the non-UTF-8 path is what's being rejected),
//!   but the message is debuggable enough for server logs and
//!   carries the same sensitivity as the full path.
//!
//! Callers that surface errors to MCP clients should log the full
//! error server-side and return a sanitised message; the
//! `query_rows` tool is the reference implementation of this
//! pattern.

use std::path::Path;

use polars::prelude::*;

/// Source format the engine can load. Each variant borrows the path
/// for the duration of the `scan` call — Polars copies it into the
/// lazy plan, so the borrow doesn't need to outlive the call.
#[derive(Debug, Clone, Copy)]
pub enum DatasetSource<'a> {
    /// Apache Parquet file. The expected on-disk format for cached
    /// datasets (see `tools_data::materialize_dataset`).
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
    /// Cap on rows the engine reads from the source before any
    /// downstream lazy ops the caller adds. `None` ⇒ no scan-time
    /// limit; the caller is responsible for bounding memory.
    ///
    /// **Not** a hard cap on the final collected row count — a
    /// downstream `join`, `explode`, or window function can still
    /// produce more rows than `n` from a `row_limit: Some(n)` scan.
    /// Callers wanting a true result-size cap need to apply their
    /// own `.limit(n)` after the heavy op (or pre-aggregate).
    /// Tools typically pass a finite limit derived from their own
    /// Definition of Done (e.g. `10_000` for `query_rows`).
    pub row_limit: Option<u32>,
}

/// Errors the engine produces. **Every** variant can carry sensitive
/// filesystem context — `Polars` embeds raw Polars output (paths,
/// schemas, offsets, column names), and `InvalidOption` embeds a
/// lossy-rendered form of the offending caller input (today: the
/// rejected dataset path via `Path::display()`, which itself is
/// lossy for non-UTF-8 — fine for server logs, not preservation).
/// Callers serialising to MCP clients should sanitise both variants
/// before responding; the module-level "Error sanitisation" docs
/// and `tools_data::query_rows` document the canonical pattern.
///
/// ## Stable contract for `Polars` messages
///
/// The engine wraps every [`PolarsError`] with an engine-owned
/// prefix before exposing it. Two views matter:
///
/// - **Inner `String` payload** (what `EngineError::Polars(s)`
///   destructures to): `<op>[ (<path>)]: <polars message>`.
/// - **`Display` output** (what `format!("{err}")` produces, the
///   form callers and tests usually compare against): adds the
///   `thiserror` variant prefix on top, so the final wire shape is
///   `polars: <op>[ (<path>)]: <polars message>`.
///
/// Either view exposes the same engine-controlled tokens. `op` is
/// one of `scan parquet`, `scan csv`, `scan ndjson`, or `collect`;
/// `path` is interpolated for scan ops only. The trailing
/// `<polars message>` stays as Polars' raw `Display` so logs keep
/// the upstream detail — but it isn't a stable test target across
/// Polars patch releases.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// Polars reader / scan / lazy-plan failure, wrapped with the
    /// engine's own context prefix (see the type-level "Stable
    /// contract" note). The prefix is engine-controlled; the
    /// remaining detail is Polars' raw `Display` and may include
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
        //
        // Polars errors are wrapped with the engine's own context
        // prefix (`scan <kind> (<path>): <polars message>`) so
        // callers/tests assert on engine-controlled wording instead
        // of upstream Polars `Display` (which isn't a stable API).
        let raw = match source {
            DatasetSource::Parquet(p) => {
                let s = utf8_path(p)?;
                LazyFrame::scan_parquet(s.into(), ScanArgsParquet::default())
                    .map_err(|e| polars_err_with_path("scan parquet", p, &e))?
            }
            DatasetSource::Csv(p) => {
                let s = utf8_path(p)?;
                LazyCsvReader::new(s.into())
                    .finish()
                    .map_err(|e| polars_err_with_path("scan csv", p, &e))?
            }
            DatasetSource::NdJson(p) => {
                let s = utf8_path(p)?;
                LazyJsonLineReader::new(s.into())
                    .finish()
                    .map_err(|e| polars_err_with_path("scan ndjson", p, &e))?
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
        lf.collect().map_err(|e| polars_err("collect", &e))
    }
}

/// Wrap a Polars error with engine-owned context. The `op` string
/// (`"scan parquet"`, `"collect"`, etc.) is the stable contract
/// callers can assert on; `err`'s `Display` follows and may shift
/// between Polars patch releases. `&err` because `Display` only
/// reads the value — taking by value would force a move at the
/// call site for no benefit.
fn polars_err(op: &str, err: &PolarsError) -> EngineError {
    EngineError::Polars(format!("{op}: {err}"))
}

/// Same as [`polars_err`] but interpolates the dataset path into the
/// engine-owned prefix — used by `scan` so logs (and tests) see the
/// path the engine was reading without depending on Polars' own
/// path formatting.
fn polars_err_with_path(op: &str, path: &Path, err: &PolarsError) -> EngineError {
    EngineError::Polars(format!("{op} ({}): {err}", path.display()))
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
        // `Result::expect` only needs `EngineError: Debug` (which it
        // has) but `expect_err` would need `LazyFrame: Debug` which
        // it doesn't have, so the Ok branch is matched out via
        // `let-else`.
        let scan_result = DatasetEngine::scan(DatasetSource::Parquet(&path), &opts);
        let Ok(lf) = scan_result else {
            panic!("scan should defer the column-mismatch to collect()");
        };
        let Err(err) = DatasetEngine::collect(lf) else {
            panic!("collect should fail for unknown column");
        };
        assert!(
            matches!(err, EngineError::Polars(_)),
            "expected Polars variant, got: {err}",
        );
        let msg = format!("{err}");
        // Assertions target only engine-owned text — the `polars:`
        // variant prefix and our own `collect:` op label. The
        // remainder of the message is Polars' raw `Display` and is
        // intentionally not asserted on (Polars wording is not a
        // stable API; see EngineError type docs).
        assert!(msg.starts_with("polars:"), "got: {msg}");
        assert!(msg.contains("collect:"), "got: {msg}");
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
        // `expect_err` would need `LazyFrame: Debug` (which it
        // doesn't have), so the Ok branch is matched out by hand.
        // Some scans validate the path eagerly (Parquet hits the
        // footer), so the error may fire here; others defer until
        // `collect()`. Try both paths and assert one of them yields
        // a `Polars` error mentioning the missing file.
        //
        // Build the path under a fresh `TempDir` rather than a
        // hard-coded `/tmp/...` literal — the literal isn't portable
        // (Windows has no `/tmp`) and could collide with a real
        // file if one happened to exist. `TempDir` exists for the
        // duration of the test and is reaped on drop, so the
        // *filename* inside it is guaranteed not to exist.
        let dir = TempDir::new().expect("tempdir");
        let filename = "__guaranteed_missing__.parquet";
        let path = dir.path().join(filename);
        let scan_result =
            DatasetEngine::scan(DatasetSource::Parquet(&path), &LoadOptions::default());
        let err = match scan_result {
            Err(e) => e,
            Ok(lf) => match DatasetEngine::collect(lf) {
                Err(e) => e,
                Ok(_) => panic!("expected scan/collect to fail for missing file"),
            },
        };
        // Assertions target only engine-controlled text — variant,
        // variant-Display prefix, and an engine op label. **No
        // assertion on Polars' raw message body**: Polars wording
        // isn't a stable API across patch releases (see EngineError
        // type docs).
        //
        // Polars `scan_parquet` is fully lazy — the missing-file
        // error fires at `collect()`, never at scan — so the op
        // label we see here is `collect`, even though the engine
        // is happy to label scan-time failures with `scan parquet`
        // when they occur eagerly (e.g. on CSV / NDJSON schema
        // inference). The OR-of-engine-labels keeps the test stable
        // against Polars deciding to validate eagerly in a future
        // version.
        assert!(
            matches!(err, EngineError::Polars(_)),
            "expected Polars variant, got: {err}",
        );
        let msg = format!("{err}");
        assert!(msg.starts_with("polars:"), "got: {msg}");
        // Substrings include the punctuation the engine writes
        // immediately after the op label — `(` for scan-with-path,
        // `:` for collect — so the test doesn't match an
        // incidentally-occurring "collect" or "scan parquet" in
        // Polars' raw message body.
        assert!(
            msg.contains("scan parquet (") || msg.contains("collect:"),
            "missing-file error must carry an engine-owned op label: {msg}",
        );
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
