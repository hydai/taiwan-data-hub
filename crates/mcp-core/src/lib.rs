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
