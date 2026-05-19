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
use mcp_core::{Dispatcher, DispatcherBuilder, McpServer};
use storage::Storage;
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

    let mut builder: DispatcherBuilder = tools_data::register_data_tools(Dispatcher::builder());
    builder = wire_db_tools_if_available(builder).await;
    let dispatcher = builder.build();
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

/// Register Postgres-backed tools when `DATABASE_URL` is set and a
/// pool can be established. Failure (unset env, bad URL, unreachable
/// pool) downgrades to "no DB tools" rather than killing the
/// process — personal-mode installs without Postgres still get a
/// working server with `list_domains`.
async fn wire_db_tools_if_available(builder: DispatcherBuilder) -> DispatcherBuilder {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        tracing::info!("DATABASE_URL unset; DB-backed tools disabled (list_domains still works)");
        return builder;
    };
    match Storage::connect(&url).await {
        Ok(storage) => {
            tracing::info!("DATABASE_URL connected; registering DB-backed tools");
            tools_data::register_db_tools(builder, storage)
        }
        Err(e) => {
            tracing::warn!(error = %e, "DATABASE_URL set but Storage::connect failed; DB tools disabled");
            builder
        }
    }
}
