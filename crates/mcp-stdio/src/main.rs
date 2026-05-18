//! stdio MCP shim for local AI clients (Claude Desktop, Cursor, Cline).
//!
//! Skeleton wiring per design issue #1.1: builds an empty [`mcp_core::Dispatcher`]
//! and serves it over rmcp's stdio transport. Subsequent issues register tools
//! against the dispatcher; this binary stays unchanged.
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

    let dispatcher = Dispatcher::default();
    let server_info = Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    let server = McpServer::new(dispatcher, server_info)
        .with_instructions("Taiwan Data Hub MCP server (skeleton — no tools registered yet).");

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting stdio MCP server"
    );

    let service = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!(error = %e, "serve error");
    })?;
    service.waiting().await?;
    Ok(())
}
