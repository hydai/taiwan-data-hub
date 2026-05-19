//! MCP data tools: list/search/get/query/materialize + rich tools.
//!
//! Tools register against an `mcp_core::DispatcherBuilder` so binaries
//! (mcp-stdio, gateway) share one dispatcher across transports. Today
//! only `list_domains` ships (#1.3); later M1 issues append to
//! [`register_data_tools`] as their tools land.

pub mod domains;
pub mod list_domains;

pub use list_domains::{ListDomainsTool, TOOL_NAME as LIST_DOMAINS_TOOL_NAME};

use mcp_core::DispatcherBuilder;

/// Register every stable data tool against `builder` and return it.
///
/// Binaries call this once at startup. Adding a new tool means appending
/// one line to this function — call sites don't need to change.
pub fn register_data_tools(builder: DispatcherBuilder) -> DispatcherBuilder {
    builder.register(ListDomainsTool::new())
}
