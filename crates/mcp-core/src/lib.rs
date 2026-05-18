//! MCP 2025-11-25 protocol abstractions and tool dispatcher.
//!
//! This crate owns the protocol surface. Concrete tools register against the
//! transport-agnostic [`Dispatcher`] in [`dispatcher`]; [`server`] wraps the
//! dispatcher with an rmcp `ServerHandler` so any transport binary (stdio,
//! HTTP/SSE, …) can serve it.
//!
//! The split keeps rmcp out of tool code: when rmcp ships a breaking change,
//! only [`server`] follows.

pub mod dispatcher;
pub mod server;

pub use dispatcher::{Dispatcher, DispatcherBuilder, ToolDescriptor, ToolError, ToolHandler};
pub use server::McpServer;

pub use rmcp;
