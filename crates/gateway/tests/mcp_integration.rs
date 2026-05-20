//! End-to-end MCP integration tests (#1.9).
//!
//! Drives the 5 base tools (`list_domains`, `search_datasets`,
//! `get_dataset`, `query_rows`, `materialize_dataset`) through the
//! same `mcp_core::Dispatcher` the gateway binary wires at runtime,
//! against a real `testcontainers-modules`-backed Postgres with the
//! project's migrations applied. Each tool gets a happy-path test
//! plus two error-case tests.
//!
//! Why go through the dispatcher rather than the full rmcp HTTP
//! stack: the dispatcher routes JSON-RPC params to the right tool
//! and serialises the result the same way the rmcp adapter does, so
//! a regression in tool wiring fails here just as loudly as it
//! would in production. The deeper protocol surface (JSON-RPC
//! framing, initialize handshake, SSE upgrade) is covered by the
//! `mcp-inspector` workflow on its own cadence.
//!
//! The `tools_list_contract_shape_is_stable` test catches rmcp
//! upgrades that break the `tools/list` wire format — it asserts
//! the structural properties our clients depend on (field names,
//! types, required keys) survive serialisation through rmcp's
//! `ListToolsResult`.
//!
//! All tests are `#[ignore]`d so the workspace-wide `cargo test`
//! stays Docker-free; run with `cargo test -p gateway -- --ignored`.

#![cfg(test)]

use std::path::PathBuf;
use std::sync::Arc;

use connectors::{DatasetMetadata, SourceId};
use mcp_core::Dispatcher;
use mcp_core::rmcp::model::{ListToolsResult, Tool};
use object_store::{LocalFsObjectStore, ObjectStore};
use polars::prelude::{ParquetWriter, df};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use storage::{
    DatasetCacheLookup, DatasetReader, DatasetSearcher, MaterializeView, Storage, UsageRecorder,
};
use tempfile::TempDir;
use testcontainers_modules::postgres::Postgres as PgContainer;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner};
use tools_data::ObjectStoreRouter;
use url::Url;

/// One-stop fixture: Postgres testcontainer + migrated `Storage` +
/// `Dispatcher` wired with all 5 base tools.
struct Harness {
    _container: ContainerAsync<PgContainer>,
    _cache_dir: TempDir,
    storage: Storage,
    dispatcher: Dispatcher,
    /// `bare-dataset` exists in the catalog but has no version /
    /// cache — `get_dataset` returns a versions-empty view and
    /// `query_rows` surfaces "not materialised yet".
    bare_dataset_slug: &'static str,
    /// Dataset that has a version + on-disk Parquet so
    /// `query_rows` can scan it.
    cached_dataset_slug: String,
    /// Separate dataset with a `dataset_files` row so the
    /// `materialize_dataset` happy-path test doesn't depend on the
    /// `query_rows` fixture.
    materialise_dataset_slug: String,
}

impl Harness {
    async fn build() -> Self {
        let container = PgContainer::default()
            .with_tag("18-alpine")
            .start()
            .await
            .expect("start postgres container");
        let host = container.get_host().await.expect("host");
        let port = container.get_host_port_ipv4(5432).await.expect("port");
        let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect");
        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .expect("migrate");
        let storage = Storage::from_pool(pool.clone());

        let domain_id = realestate_land_id(&pool).await;
        let cache_dir = TempDir::new().expect("tempdir");

        // Dataset A: bare (no versions yet) — exercises `get_dataset`
        // not-found-version paths.
        storage
            .upsert_dataset(
                domain_id,
                SourceId::DataGovTw,
                &sample_metadata("bare", "bare-dataset"),
            )
            .await
            .expect("upsert bare");

        // Dataset B: has a version + an on-disk Parquet, cached so
        // `query_rows` can scan it.
        let cached_meta = sample_metadata("cached", "cached-dataset");
        let cached_dataset_id = storage
            .upsert_dataset(domain_id, SourceId::DataGovTw, &cached_meta)
            .await
            .expect("upsert cached");
        let version_id = storage
            .record_version_if_changed(cached_dataset_id, "2026-05-01", "sha256:cached-1")
            .await
            .expect("version")
            .expect("inserted");
        let parquet_path = write_fixture_parquet(cache_dir.path()).await;
        // Store the bare filesystem path rather than `format!(
        // "file://{path}")`. The `file://` concatenation produces
        // `file:////tmp/...` on Unix (path already starts with `/`),
        // which is technically a valid URI but non-canonical and
        // platform-quirky. Both `query_rows` (strips `file://` then
        // falls through to `PathBuf::from`) and `LocalFsObjectStore`
        // (same) accept bare paths, so the simpler form is also the
        // correct one.
        let cache_uri = parquet_path.display().to_string();
        sqlx::query("UPDATE datasets SET cached = true, cache_path = $1 WHERE id = $2")
            .bind(&cache_uri)
            .bind(cached_dataset_id)
            .execute(&pool)
            .await
            .expect("set cache_path");
        // Register a dataset_files row so materialize_dataset can
        // resolve a file URI for the cached dataset too (one row
        // covers both flows in our fixture set).
        sqlx::query(
            "INSERT INTO dataset_files (dataset_version_id, format, uri, byte_size, checksum) \
             VALUES ($1, 'parquet', $2, $3, 'sha256:cached-1')",
        )
        .bind(version_id)
        .bind(&cache_uri)
        .bind(byte_size_of(&parquet_path))
        .execute(&pool)
        .await
        .expect("insert file");

        // Dataset C: has a version + a dataset_files row at a
        // separate path so `materialize_dataset` happy-path tests
        // don't depend on the query_rows fixture.
        let materialise_meta = sample_metadata("materialise", "materialise-dataset");
        let materialise_dataset_id = storage
            .upsert_dataset(domain_id, SourceId::DataGovTw, &materialise_meta)
            .await
            .expect("upsert materialise");
        let mver = storage
            .record_version_if_changed(materialise_dataset_id, "2026-05-01", "sha256:mat-1")
            .await
            .expect("version")
            .expect("inserted");
        let mfile = write_fixture_parquet_named(cache_dir.path(), "materialise.parquet").await;
        // Bare filesystem path — see the `cache_uri` comment above
        // for why we don't pre-format with `file://`.
        let muri = mfile.display().to_string();
        sqlx::query(
            "INSERT INTO dataset_files (dataset_version_id, format, uri, byte_size, checksum) \
             VALUES ($1, 'parquet', $2, $3, 'sha256:mat-1')",
        )
        .bind(mver)
        .bind(&muri)
        .bind(byte_size_of(&mfile))
        .execute(&pool)
        .await
        .expect("insert materialise file");

        let dispatcher = build_dispatcher(&storage);

        Self {
            _container: container,
            _cache_dir: cache_dir,
            storage,
            dispatcher,
            bare_dataset_slug: "bare-dataset",
            cached_dataset_slug: cached_meta.slug.clone(),
            materialise_dataset_slug: materialise_meta.slug.clone(),
        }
    }
}

/// Build the dispatcher with every base tool registered, mirroring
/// `gateway::wire_db_tools_if_available`. Production wires
/// `LocalFsObjectStore` via env vars; tests pass a hard-coded base
/// URL + secret because both are deterministic + private to the
/// test.
fn build_dispatcher(storage: &Storage) -> Dispatcher {
    let local_fs: Arc<dyn ObjectStore> = Arc::new(
        LocalFsObjectStore::new(
            Url::parse("http://localhost:18080").unwrap(),
            b"integration-test-secret-must-be-at-least-32-bytes".to_vec(),
        )
        .expect("build LocalFsObjectStore"),
    );
    let router = ObjectStoreRouter::new().with_local_fs(local_fs);

    let reader: Arc<dyn DatasetReader> = Arc::new(storage.clone());
    let searcher: Arc<dyn DatasetSearcher> = Arc::new(storage.clone());
    let cache: Arc<dyn DatasetCacheLookup> = Arc::new(storage.clone());
    let view: Arc<dyn MaterializeView> = Arc::new(storage.clone());
    let recorder: Arc<dyn UsageRecorder> = Arc::new(storage.clone());

    let builder = Dispatcher::builder();
    let builder = tools_data::register_data_tools(builder);
    let builder = tools_data::register_db_tools_with(
        builder, reader, searcher, cache, view, recorder, router,
    );
    builder.build()
}

fn sample_metadata(source_id: &str, slug: &str) -> DatasetMetadata {
    DatasetMetadata {
        source_id: source_id.to_owned(),
        slug: slug.to_owned(),
        title_i18n: std::collections::BTreeMap::from([
            ("zh-TW".to_owned(), format!("資料集-{slug}")),
            ("en".to_owned(), format!("Dataset {slug}")),
        ]),
        description_i18n: std::collections::BTreeMap::from([(
            "zh-TW".to_owned(),
            format!("整合測試用資料集 {slug}"),
        )]),
        license: "政府資料開放授權條款-第1版".to_owned(),
        publisher: Some("整合測試".to_owned()),
        update_frequency: Some("daily".to_owned()),
        original_url: Some(format!("https://example.com/{slug}")),
        last_modified_at: chrono::DateTime::parse_from_rfc3339("2026-04-15T03:30:00Z")
            .ok()
            .map(|d| d.with_timezone(&chrono::Utc)),
        upstream_categories: vec!["不動產與土地".to_owned()],
    }
}

async fn realestate_land_id(pool: &PgPool) -> i16 {
    let row: (i16,) = sqlx::query_as("SELECT id FROM domains WHERE slug = 'realestate-land'")
        .fetch_one(pool)
        .await
        .expect("seeded domain present");
    row.0
}

/// Write the standard 3-row fixture used by `query_rows` tests.
async fn write_fixture_parquet(dir: &std::path::Path) -> PathBuf {
    write_fixture_parquet_named(dir, "fixture.parquet").await
}

async fn write_fixture_parquet_named(dir: &std::path::Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    // The polars writer is blocking; hop to spawn_blocking so the
    // tokio runtime isn't tied up on what amounts to a sync syscall.
    let p = path.clone();
    tokio::task::spawn_blocking(move || {
        let mut df = df! {
            "id" => &[1_i64, 2, 3],
            "name" => &["alice", "bob", "carol"],
            "score" => &[10.5_f64, 12.0, 7.25],
        }
        .expect("build df");
        let file = std::fs::File::create(&p).expect("create parquet");
        ParquetWriter::new(file).finish(&mut df).expect("write");
    })
    .await
    .expect("spawn_blocking");
    path
}

fn byte_size_of(path: &std::path::Path) -> i64 {
    std::fs::metadata(path)
        .expect("stat fixture")
        .len()
        .try_into()
        .expect("fixture small enough for i64")
}

/// One shared marker so each individual test reads as "harness +
/// call + assert" rather than re-stamping the ignore reason.
macro_rules! mcp_integration_test {
    ($name:ident, $body:expr) => {
        #[tokio::test(flavor = "current_thread")]
        #[ignore = "requires docker; run with `cargo test -p gateway -- --ignored`"]
        async fn $name() {
            let h = Harness::build().await;
            let body: fn(
                &Harness,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> = $body;
            body(&h).await;
        }
    };
}

// ─────────────────────────────── list_domains ───────────────────────────────

mcp_integration_test!(list_domains_happy_returns_20_seeded_domains, |h| {
    Box::pin(async move {
        let res = h
            .dispatcher
            .call_tool("list_domains", json!({}))
            .await
            .expect("list_domains happy");
        let domains = res["domains"].as_array().expect("domains array");
        // 20 domains come from `tools-data/config/domains.yaml`
        // (embedded at compile time). The DB migration 0002 also
        // seeds a `domains` table but `list_domains` reads from
        // the embedded YAML for fast in-memory lookup — that's
        // the canonical source for this tool.
        assert_eq!(
            domains.len(),
            20,
            "20 domains in embedded config/domains.yaml"
        );
        // `list_domains` resolves i18n server-side — each domain
        // surfaces a plain locale-rendered `name`, not the raw
        // i18n object. zh-TW is the default; assert that path.
        assert_eq!(res["locale"], "zh-TW");
        for d in domains {
            assert!(d["slug"].is_string());
            assert!(d["name"].is_string());
            assert!(d["sort_order"].is_number());
        }
    })
});

mcp_integration_test!(list_domains_unknown_locale_falls_back_to_zh_tw, |h| {
    Box::pin(async move {
        // Unknown locale tags are accepted; missing keys fall back
        // to zh-TW per `COALESCE(col->>$lang, col->>'zh-TW')`. The
        // response still echoes the requested locale label even
        // though every value resolved to the zh-TW fallback.
        let res = h
            .dispatcher
            .call_tool("list_domains", json!({"locale": "fr-FR"}))
            .await
            .expect("list_domains with unknown locale");
        assert_eq!(res["locale"], "fr-FR");
        assert_eq!(res["domains"].as_array().unwrap().len(), 20);
    })
});

mcp_integration_test!(list_domains_rejects_non_string_locale, |h| {
    Box::pin(async move {
        let err = h
            .dispatcher
            .call_tool("list_domains", json!({"locale": 42}))
            .await
            .expect_err("non-string locale must be InvalidArguments");
        assert!(matches!(err, mcp_core::ToolError::InvalidArguments(_)));
    })
});

// ─────────────────────────────── search_datasets ────────────────────────────

mcp_integration_test!(search_datasets_happy_returns_seeded_datasets, |h| {
    Box::pin(async move {
        let res = h
            .dispatcher
            .call_tool("search_datasets", json!({"limit": 20}))
            .await
            .expect("search happy");
        let hits = res["hits"].as_array().expect("hits array");
        assert!(
            hits.len() >= 3,
            "fixture inserts 3 datasets (bare, cached, materialise); got {}",
            hits.len()
        );
    })
});

mcp_integration_test!(search_datasets_rejects_negative_offset, |h| {
    Box::pin(async move {
        let err = h
            .dispatcher
            .call_tool("search_datasets", json!({"offset": -1}))
            .await
            .expect_err("negative offset must be rejected");
        assert!(matches!(err, mcp_core::ToolError::InvalidArguments(_)));
    })
});

mcp_integration_test!(search_datasets_unknown_filter_returns_empty, |h| {
    Box::pin(async move {
        // A domain slug that doesn't exist returns zero hits, not an
        // error. Pinning this so a future refactor that turns "no
        // match" into a NotFound breaks visibly.
        let res = h
            .dispatcher
            .call_tool(
                "search_datasets",
                json!({"domain": "definitely-not-a-domain"}),
            )
            .await
            .expect("search with unknown domain succeeds");
        assert_eq!(res["hits"].as_array().unwrap().len(), 0);
    })
});

// ─────────────────────────────── get_dataset ────────────────────────────────

mcp_integration_test!(get_dataset_happy_returns_full_view, |h| {
    Box::pin(async move {
        let res = h
            .dispatcher
            .call_tool(
                "get_dataset",
                json!({"slug": h.cached_dataset_slug.clone()}),
            )
            .await
            .expect("get_dataset happy");
        assert_eq!(res["dataset"]["slug"], h.cached_dataset_slug);
        let versions = res["versions"].as_array().expect("versions array");
        assert_eq!(versions.len(), 1);
        assert!(!versions[0]["files"].as_array().unwrap().is_empty());
    })
});

mcp_integration_test!(get_dataset_returns_not_found_for_unknown_slug, |h| {
    Box::pin(async move {
        let err = h
            .dispatcher
            .call_tool("get_dataset", json!({"slug": "no-such-dataset"}))
            .await
            .expect_err("unknown slug must be NotFound");
        assert!(matches!(err, mcp_core::ToolError::NotFound(_)));
    })
});

mcp_integration_test!(get_dataset_requires_id_or_slug, |h| {
    Box::pin(async move {
        let err = h
            .dispatcher
            .call_tool("get_dataset", json!({}))
            .await
            .expect_err("missing id+slug must be InvalidArguments");
        assert!(matches!(err, mcp_core::ToolError::InvalidArguments(_)));
    })
});

// ─────────────────────────────── query_rows ─────────────────────────────────

mcp_integration_test!(query_rows_happy_returns_rows_columns_and_flag, |h| {
    Box::pin(async move {
        let res = h
            .dispatcher
            .call_tool(
                "query_rows",
                json!({
                    "slug": h.cached_dataset_slug.clone(),
                    "sql": "SELECT id, name FROM current_dataset ORDER BY id LIMIT 2",
                }),
            )
            .await
            .expect("query_rows happy");
        let cols = res["columns"].as_array().unwrap();
        assert_eq!(
            cols.iter().map(|v| v.as_str().unwrap()).collect::<Vec<_>>(),
            vec!["id", "name"]
        );
        let rows = res["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        // The 3-row fixture has `id IN (1, 2, 3)`. ORDER BY id +
        // LIMIT 2 yields the first two — alice + bob.
        assert_eq!(rows[0][1], "alice");
        assert_eq!(rows[1][1], "bob");
    })
});

mcp_integration_test!(query_rows_rejects_unknown_table, |h| {
    Box::pin(async move {
        let err = h
            .dispatcher
            .call_tool(
                "query_rows",
                json!({
                    "slug": h.cached_dataset_slug.clone(),
                    "sql": "SELECT * FROM secrets",
                }),
            )
            .await
            .expect_err("unknown table must be rejected");
        assert!(matches!(err, mcp_core::ToolError::InvalidArguments(_)));
    })
});

mcp_integration_test!(
    query_rows_returns_not_found_when_dataset_not_materialised,
    |h| {
        Box::pin(async move {
            // `bare-dataset` has no cached Parquet — the cache lookup
            // returns Some(_) with cached=false, which the tool maps to
            // a NotFound with a helpful "call materialize_dataset first"
            // message.
            let err = h
                .dispatcher
                .call_tool(
                    "query_rows",
                    json!({
                        "slug": h.bare_dataset_slug,
                        "sql": "SELECT 1 FROM current_dataset",
                    }),
                )
                .await
                .expect_err("non-materialised dataset must be NotFound");
            assert!(matches!(err, mcp_core::ToolError::NotFound(_)));
        })
    }
);

// ─────────────────────────────── materialize_dataset ────────────────────────

mcp_integration_test!(materialize_dataset_happy_returns_signed_url, |h| {
    Box::pin(async move {
        let res = h
            .dispatcher
            .call_tool(
                "materialize_dataset",
                json!({
                    "slug": h.materialise_dataset_slug.clone(),
                    "format": "parquet",
                    "ttl_seconds": 60,
                }),
            )
            .await
            .expect("materialize happy");
        assert_eq!(res["slug"], h.materialise_dataset_slug);
        assert_eq!(res["format"], "parquet");
        let url = res["url"].as_str().expect("url present");
        assert!(
            url.starts_with("http://localhost:18080/files/dl/"),
            "url: {url}"
        );
        assert!(url.contains("expires="));
        assert!(url.contains("sig="));
        // Audit row should land for the happy path.
        let count: (i64,) =
            sqlx::query_as("SELECT count(*) FROM usage_records WHERE tool = 'materialize_dataset'")
                .fetch_one(h.storage.pool())
                .await
                .expect("audit count");
        assert!(count.0 >= 1, "expected at least one usage_records row");
    })
});

mcp_integration_test!(
    materialize_dataset_returns_not_found_for_unknown_slug,
    |h| {
        Box::pin(async move {
            let err = h
                .dispatcher
                .call_tool("materialize_dataset", json!({"slug": "no-such-dataset"}))
                .await
                .expect_err("unknown slug must be NotFound");
            assert!(matches!(err, mcp_core::ToolError::NotFound(_)));
        })
    }
);

mcp_integration_test!(materialize_dataset_rejects_ttl_below_minimum, |h| {
    Box::pin(async move {
        let err = h
            .dispatcher
            .call_tool(
                "materialize_dataset",
                json!({
                    "slug": h.materialise_dataset_slug.clone(),
                    "ttl_seconds": 5,
                }),
            )
            .await
            .expect_err("ttl below MIN_TTL must be InvalidArguments");
        assert!(matches!(err, mcp_core::ToolError::InvalidArguments(_)));
    })
});

// ─────────────────────────────── contract test ──────────────────────────────

mcp_integration_test!(tools_list_contract_shape_is_stable, |h| {
    Box::pin(async move {
        // Build the same `ListToolsResult` the rmcp adapter produces
        // (`server.rs` does this transformation per request) and
        // serialise it. Going through rmcp's `Tool` + `ListToolsResult`
        // serialisation is what catches upgrade-induced wire-format
        // drift: if rmcp ever renames `inputSchema` or changes the
        // shape of `Tool`, the JSON below stops matching the asserted
        // structural invariants. We skip the `ServerHandler::list_tools`
        // call because rmcp's `RequestContext` has no public
        // constructor; the bytes on the wire are the same either way.
        let descriptors = h.dispatcher.list_tools();
        let tools: Vec<Tool> = descriptors
            .into_iter()
            .map(|d| {
                let mut tool = Tool::new(d.name, d.description, Arc::new(d.input_schema));
                if let Some(out) = d.output_schema {
                    tool.output_schema = Some(Arc::new(out));
                }
                tool
            })
            .collect();
        let list = ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        };
        let json: Value = serde_json::to_value(&list).expect("serialise");

        let tools = json["tools"].as_array().expect("tools array");
        let names: std::collections::BTreeSet<&str> = tools
            .iter()
            .map(|t| t["name"].as_str().expect("name"))
            .collect();
        let expected: std::collections::BTreeSet<&str> = [
            "list_domains",
            "search_datasets",
            "get_dataset",
            "query_rows",
            "materialize_dataset",
        ]
        .into_iter()
        .collect();
        assert_eq!(names, expected, "exactly the 5 base tools must register");

        // Per-tool structural invariants. rmcp's Tool serialises
        // `inputSchema` (camelCase) per the MCP spec; if a future
        // rmcp release changes this, the assertion below catches it.
        for tool in tools {
            assert!(tool["name"].is_string());
            assert!(tool["description"].is_string());
            assert!(
                tool["inputSchema"].is_object(),
                "tool {} missing inputSchema; got {tool}",
                tool["name"]
            );
            assert_eq!(
                tool["inputSchema"]["type"], "object",
                "inputSchema must declare type=object"
            );
        }

        // Every base tool declares an output schema — clients use
        // this to decide whether to expect structured content.
        // Asserting universal coverage here catches the regression
        // shape of "a tool stops declaring its output schema" just
        // as well as the inverse.
        let with_output: std::collections::BTreeSet<&str> = tools
            .iter()
            .filter(|t| t["outputSchema"].is_object())
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            with_output,
            names,
            "every base tool must declare an outputSchema; missing: {:?}",
            names.difference(&with_output).collect::<Vec<_>>(),
        );
    })
});
