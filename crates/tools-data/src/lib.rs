//! MCP data tools: list/search/get/query/materialize + rich tools.
//!
//! Tools register against an `mcp_core::DispatcherBuilder` in two
//! tiers:
//!
//! - [`register_data_tools`] — always-on tools that need no
//!   database (today: `list_domains`).
//! - [`register_db_tools`] — tools backed by a
//!   `storage::DatasetReader`. Binaries call this only when a
//!   `DATABASE_URL` is configured and the pool connects
//!   successfully. A personal-mode install without Postgres simply
//!   ships fewer tools.

pub mod domains;
pub mod get_dataset;
pub mod list_domains;

pub use get_dataset::{GetDatasetTool, TOOL_NAME as GET_DATASET_TOOL_NAME};
pub use list_domains::{ListDomainsTool, TOOL_NAME as LIST_DOMAINS_TOOL_NAME};

use mcp_core::DispatcherBuilder;
use storage::DatasetReader;

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

/// Register every tool that needs a `DatasetReader` (i.e. Postgres).
///
/// `reader` is typed as a `storage::DatasetReader`, not a concrete
/// `storage::Storage`, so callers can plug in a test stub when
/// scripting an MCP scenario without a live database.
pub fn register_db_tools<R: DatasetReader>(
    builder: DispatcherBuilder,
    reader: R,
) -> DispatcherBuilder {
    builder.register(GetDatasetTool::new(reader))
}
