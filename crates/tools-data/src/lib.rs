//! MCP data tools: list/search/get/query/materialize + rich tools.
//!
//! Tools register against an `mcp_core::DispatcherBuilder` in two
//! tiers:
//!
//! - [`register_data_tools`] — always-on tools that need no
//!   database (today: `list_domains`).
//! - [`register_db_tools`] — tools that need Postgres. Takes a
//!   concrete [`storage::Storage`] handle and wires every tool that
//!   implements its narrowest required trait (today: `get_dataset`
//!   via [`storage::DatasetReader`], `search_datasets` via
//!   [`storage::DatasetSearcher`], and `query_rows` via
//!   [`storage::DatasetCacheLookup`]). Binaries call this only when
//!   `DATABASE_URL` is configured and the pool connects; a personal-
//!   mode install without Postgres simply ships fewer tools.
//! - [`register_db_tools_with`] — lower-level entry point that takes
//!   the trait objects (`Arc<dyn DatasetReader>` etc.) directly so
//!   tests can plug in in-memory stubs per trait without going
//!   through `Storage`.

pub mod describe_schema;
pub mod domains;
pub mod get_dataset;
pub mod get_sample;
pub mod join_datasets;
pub mod list_domains;
pub mod materialize_dataset;
pub mod query_rows;
pub mod search_datasets;
pub mod sql_guard;

pub use describe_schema::{DescribeSchemaTool, TOOL_NAME as DESCRIBE_SCHEMA_TOOL_NAME};
pub use get_dataset::{GetDatasetTool, TOOL_NAME as GET_DATASET_TOOL_NAME};
pub use get_sample::{GetSampleTool, TOOL_NAME as GET_SAMPLE_TOOL_NAME};
pub use join_datasets::{JoinDatasetsTool, TOOL_NAME as JOIN_DATASETS_TOOL_NAME};
pub use list_domains::{ListDomainsTool, TOOL_NAME as LIST_DOMAINS_TOOL_NAME};
pub use materialize_dataset::{
    MaterializeDatasetTool, ObjectStoreRouter, TOOL_NAME as MATERIALIZE_DATASET_TOOL_NAME,
};
pub use query_rows::{QueryRowsTool, TOOL_NAME as QUERY_ROWS_TOOL_NAME};
pub use search_datasets::{SearchDatasetsTool, TOOL_NAME as SEARCH_DATASETS_TOOL_NAME};

use mcp_core::DispatcherBuilder;
use std::sync::Arc;
use storage::{
    DatasetCacheLookup, DatasetReader, DatasetSearcher, MaterializeView, Storage, UsageRecorder,
};

/// Register every data tool that has no runtime dependencies.
///
/// Adding a new always-on tool means appending one line to this
/// function — call sites don't need to change.
///
/// As a side effect this warms the embedded-YAML cache so a
/// malformed `config/domains.yaml` panics at process boot rather
/// than at the first `list_domains` call.
pub fn register_data_tools(builder: DispatcherBuilder) -> DispatcherBuilder {
    let _ = domains::embedded();
    builder.register(ListDomainsTool::new())
}

/// Register every tool backed by a Postgres-shaped storage handle.
/// In production the binary passes a single [`Storage`] that already
/// implements every trait we need; tests can plug in separate stubs
/// per trait via the lower-level [`register_db_tools_with`] entry
/// point.
///
/// `router` is the [`ObjectStoreRouter`] that backs
/// `materialize_dataset` — production wires `LocalFsObjectStore` and
/// `S3ObjectStore` per the deployment shape; tests / personal-mode
/// without an object store can pass `ObjectStoreRouter::new()` and
/// the tool will still register (calls will surface a clear "no
/// backend configured" error).
pub fn register_db_tools(
    builder: DispatcherBuilder,
    storage: Storage,
    router: ObjectStoreRouter,
) -> DispatcherBuilder {
    // Wrap once so the per-tool `Arc<dyn Trait>` re-counts cheaply
    // instead of cloning the inner `Storage` (which is itself Arc-
    // backed but the trait-object wrapping needs to happen at this
    // boundary).
    let reader: Arc<dyn DatasetReader> = Arc::new(storage.clone());
    let searcher: Arc<dyn DatasetSearcher> = Arc::new(storage.clone());
    let cache: Arc<dyn DatasetCacheLookup> = Arc::new(storage.clone());
    let view: Arc<dyn MaterializeView> = Arc::new(storage.clone());
    let recorder: Arc<dyn UsageRecorder> = Arc::new(storage);
    register_db_tools_with(builder, reader, searcher, cache, view, recorder, router)
}

/// Lower-level entry point that takes the trait objects directly, so
/// tests can mix and match in-memory stubs without going through
/// `Storage`. Each tool needs only the narrowest trait it uses, so
/// future additions just take another `Arc<dyn …>` parameter.
pub fn register_db_tools_with(
    builder: DispatcherBuilder,
    reader: Arc<dyn DatasetReader>,
    searcher: Arc<dyn DatasetSearcher>,
    cache: Arc<dyn DatasetCacheLookup>,
    view: Arc<dyn MaterializeView>,
    recorder: Arc<dyn UsageRecorder>,
    router: ObjectStoreRouter,
) -> DispatcherBuilder {
    builder
        .register(GetDatasetTool::from_arc(reader))
        .register(SearchDatasetsTool::from_arc(searcher))
        .register(QueryRowsTool::from_arc(cache.clone()))
        .register(DescribeSchemaTool::from_arc(cache.clone()))
        .register(GetSampleTool::from_arc(cache.clone()))
        .register(JoinDatasetsTool::from_arc(cache))
        .register(MaterializeDatasetTool::from_arcs(
            view,
            recorder,
            Arc::new(router),
        ))
}
