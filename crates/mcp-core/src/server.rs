//! rmcp `ServerHandler` adapter over the transport-agnostic [`Dispatcher`].
//!
//! Keep the surface area thin: the only rmcp types referenced outside this
//! module are re-exported under `mcp_core::rmcp` so tool implementations
//! stay rmcp-free.

use std::sync::Arc;

use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, ListToolsResult,
    PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer};

use crate::dispatcher::{Dispatcher, ToolError};

/// rmcp-facing MCP server. Cheap to clone; routes incoming tool calls to the
/// inner [`Dispatcher`].
///
/// Binaries should pass their own [`Implementation`] via [`McpServer::new`] —
/// `Implementation::from_build_env()` resolves at rmcp's compile time, not the
/// caller's, so using it here would always report `name: "rmcp"`.
#[derive(Clone, Debug)]
pub struct McpServer {
    dispatcher: Dispatcher,
    server_info: Implementation,
    instructions: Option<String>,
}

impl McpServer {
    pub fn new(dispatcher: Dispatcher, server_info: Implementation) -> Self {
        Self {
            dispatcher,
            server_info,
            instructions: None,
        }
    }

    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    pub fn dispatcher(&self) -> &Dispatcher {
        &self.dispatcher
    }
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        let info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(self.server_info.clone())
            .with_protocol_version(ProtocolVersion::V_2025_11_25);
        match &self.instructions {
            Some(text) => info.with_instructions(text.clone()),
            None => info,
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let tools = self
            .dispatcher
            .list_tools()
            .into_iter()
            .map(|d| Tool::new(d.name, d.description, Arc::new(d.input_schema)))
            .collect();
        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        // MCP `arguments` is optional. Map absence to an empty object so
        // tools that pattern-match with `.as_object()` / `.get(..)` don't
        // see `null` for a legitimate no-arg call.
        let args = serde_json::Value::Object(request.arguments.unwrap_or_default());
        match self.dispatcher.call_tool(&request.name, args).await {
            Ok(value) => {
                let text = serde_json::to_string(&value)
                    .unwrap_or_else(|_| "<unserializable result>".into());
                Ok(CallToolResult::success(vec![Content::text(text)]))
            }
            Err(ToolError::NotFound(name)) => Err(McpError::invalid_params(
                format!("unknown tool: {name}"),
                None,
            )),
            Err(ToolError::InvalidArguments(msg)) => Err(McpError::invalid_params(msg, None)),
            Err(ToolError::Execution(msg)) => Err(McpError::internal_error(msg, None)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_impl() -> Implementation {
        Implementation::new("test-server", "0.0.1")
    }

    #[test]
    fn empty_server_advertises_tools_capability_and_spec_version() {
        let server = McpServer::new(Dispatcher::default(), test_impl());
        let info = server.get_info();
        assert_eq!(info.protocol_version, ProtocolVersion::V_2025_11_25);
        assert!(info.capabilities.tools.is_some());
    }

    #[test]
    fn server_info_reflects_caller_identity_not_rmcp() {
        let server = McpServer::new(Dispatcher::default(), test_impl());
        let info = server.get_info();
        assert_eq!(info.server_info.name, "test-server");
        assert_eq!(info.server_info.version, "0.0.1");
    }

    #[test]
    fn instructions_are_attached_when_provided() {
        let server = McpServer::new(Dispatcher::default(), test_impl()).with_instructions("hello");
        let info = server.get_info();
        assert_eq!(info.instructions.as_deref(), Some("hello"));
    }
}
