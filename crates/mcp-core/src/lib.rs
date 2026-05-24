//! MCP 2025-11-25 protocol abstractions and tool dispatcher.
//!
//! This crate owns the protocol surface. Concrete tools register against the
//! transport-agnostic [`Dispatcher`] in [`dispatcher`]; [`server`] wraps the
//! dispatcher with an rmcp `ServerHandler` so any transport binary (stdio,
//! HTTP/SSE, …) can serve it.
//!
//! The split keeps rmcp out of tool code: when rmcp ships a breaking change,
//! only [`server`] follows.
//!
//! [`dataset_engine`] is the Polars `LazyFrame` helper shared by the
//! rich MCP tools (M3 #3.2–#3.5). It lives here rather than in
//! `tools-data` because the engine is a core capability of the MCP
//! server, not a per-tool implementation detail.

pub mod dataset_engine;
pub mod dispatcher;
pub mod server;

pub use dataset_engine::{DatasetEngine, DatasetSource, EngineError, LoadOptions};
pub use dispatcher::{Dispatcher, DispatcherBuilder, ToolDescriptor, ToolError, ToolHandler};
pub use server::McpServer;

pub use rmcp;

/// Wire-format string of the MCP protocol version the server speaks.
///
/// Single source of truth shared by [`McpServer`] (which advertises
/// it in the `initialize` response via
/// `rmcp::model::ProtocolVersion::V_2025_11_25`) and any out-of-band
/// surface that has to publish the same value — e.g. the gateway's
/// `/.well-known/mcp.json` manifest. Pinning the literal here +
/// a debug-time assertion against the rmcp constant means an SDK
/// upgrade that bumps the version surfaces as a test failure
/// rather than silent drift between surfaces.
pub const PROTOCOL_VERSION: &str = "2025-11-25";

#[cfg(test)]
mod protocol_version_tests {
    use super::*;
    use rmcp::model::ProtocolVersion;

    #[test]
    fn pinned_version_matches_rmcp_constant() {
        // Guard against rmcp bumping `V_2025_11_25` to a different
        // string OR our `PROTOCOL_VERSION` constant being updated
        // without the rmcp dep pin. Either case is a real drift
        // bug — the manifest at `/.well-known/mcp.json` would
        // advertise a version different from what the live `/mcp`
        // initialize response negotiates.
        assert_eq!(PROTOCOL_VERSION, ProtocolVersion::V_2025_11_25.as_str());
    }
}
