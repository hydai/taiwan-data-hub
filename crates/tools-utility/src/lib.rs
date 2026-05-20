//! Taiwan-specific utility tools: ID validation (this milestone),
//! address normalizer (#3.7), ROC date utilities (#3.8), canonicalizers
//! and code dictionaries (#3.10–#3.12). Each tool implements
//! [`mcp_core::ToolHandler`] and is registered with the dispatcher via
//! [`register_utility_tools`].
//!
//! All utilities are state-free pure functions exposed both as a
//! native Rust API (for direct use by other Rust crates / future REST
//! handlers) and as MCP tools (via the wrapper modules in this
//! crate). Keeping the two layers separate means MCP wiring can be
//! unit-tested without spinning up rmcp, and Rust callers don't pay
//! for `serde_json::Value` round-trips.

pub mod national_id;
pub mod passport;
pub mod tax_id;
pub mod validate_id_tool;

pub use validate_id_tool::{TOOL_NAME as TW_VALIDATE_ID_TOOL_NAME, ValidateIdTool};

use mcp_core::DispatcherBuilder;

/// Register every utility tool with the supplied dispatcher builder.
///
/// Adding a new utility tool means appending one line to this
/// function — call sites in `mcp-stdio` and `gateway` don't need to
/// change.
pub fn register_utility_tools(builder: DispatcherBuilder) -> DispatcherBuilder {
    builder.register(ValidateIdTool::new())
}
