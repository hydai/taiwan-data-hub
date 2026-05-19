//! stdio MCP shim for local AI clients (Claude Desktop, Cursor, Cline).
//!
//! Builds an `mcp_core::Dispatcher` seeded by `tools_data::register_data_tools`
//! and serves it over rmcp's stdio transport. The gateway crate wires the
//! same registration helper for HTTP — adding a new tool there means
//! editing one function in `tools-data`, not both binaries.
//!
//! Run via `cargo run -p mcp-stdio` (or the Inspector wrapper:
//! `npx @modelcontextprotocol/inspector cargo run -p mcp-stdio`).

use anyhow::Result;
use mcp_core::rmcp::model::Implementation;
use mcp_core::rmcp::{ServiceExt, transport::stdio};
use mcp_core::{Dispatcher, McpServer};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // MCP clients own stdout for protocol bytes — every log MUST go to stderr.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let dispatcher = tools_data::register_data_tools(Dispatcher::builder()).build();
    let tool_count = dispatcher.len();
    let server_info = Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    let server =
        McpServer::new(dispatcher, server_info).with_instructions("Taiwan Data Hub MCP server.");

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        tools = tool_count,
        "starting stdio MCP server"
    );

    let service = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!(error = %e, "serve error");
    })?;
    service.waiting().await?;
    Ok(())
}
