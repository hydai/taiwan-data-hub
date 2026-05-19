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
            .map(|d| {
                // Tool is #[non_exhaustive] in rmcp 1.x, but its fields are
                // `pub` — construct via `Tool::new` then attach the optional
                // output schema by field assignment. rmcp's own
                // `with_output_schema<T: JsonSchema>` derives from a Rust
                // type; we already carry the schema as a `JsonObject`, so
                // assigning directly skips the schemars round-trip.
                let mut tool = Tool::new(d.name, d.description, Arc::new(d.input_schema));
                if let Some(out) = d.output_schema {
                    tool.output_schema = Some(Arc::new(out));
                }
                tool
            })
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

        // A tool that declared `output_schema` MUST return structured
        // content per the MCP spec — otherwise rmcp returns -32600 to the
        // client. Decide which `CallToolResult` constructor to use by
        // peeking at the descriptor before dispatching.
        let expects_structured = self
            .dispatcher
            .descriptor(&request.name)
            .is_some_and(|d| d.output_schema.is_some());

        let outcome = self.dispatcher.call_tool(&request.name, args).await;
        package_tool_outcome(&request.name, expects_structured, outcome)
    }
}

/// Translate a dispatcher outcome into the matching rmcp `CallToolResult`
/// or `McpError`, applying the MCP spec rules around `output_schema`:
///
/// * tools that declared an output schema MUST return structured content
///   (a JSON *object*) — anything else trips `-32603 Internal error`,
///   surfaced from mcp-core rather than rmcp's deeper validator;
/// * tools without an output schema render results as plain text via
///   [`value_to_content`].
///
/// Factored out of `ServerHandler::call_tool` so the routing contract is
/// unit-testable without needing to construct an rmcp `RequestContext`.
fn package_tool_outcome(
    name: &str,
    expects_structured: bool,
    outcome: Result<serde_json::Value, ToolError>,
) -> Result<CallToolResult, McpError> {
    match outcome {
        Ok(value) if expects_structured => {
            if !value.is_object() {
                return Err(McpError::internal_error(
                    format!(
                        "tool `{name}` declares output_schema but returned a non-object \
                         top-level value"
                    ),
                    None,
                ));
            }
            Ok(CallToolResult::structured(value))
        }
        Ok(value) => Ok(CallToolResult::success(vec![value_to_content(value)])),
        // `NotFound` covers both "this tool isn't registered" (the
        // dispatcher's case) and "the resource you asked for doesn't
        // exist" (a tool's own case, e.g. `get_dataset(slug=nope)`).
        // `InvalidArguments` is the same MCP error class from the
        // client's perspective — `-32602 Invalid params` — and the
        // inner message disambiguates. Pass either through unchanged.
        Err(ToolError::NotFound(msg) | ToolError::InvalidArguments(msg)) => {
            Err(McpError::invalid_params(msg, None))
        }
        Err(ToolError::Execution(msg)) => Err(McpError::internal_error(msg, None)),
    }
}

/// Render a tool's `serde_json::Value` result as MCP text content.
///
/// `Value::String("hi")` becomes the literal text `hi`. Any other variant
/// gets JSON-serialized so structured results survive the transport
/// faithfully. Serialization is infallible for `Value`, but we guard
/// defensively because `to_string` returns a `Result`.
fn value_to_content(value: serde_json::Value) -> Content {
    match value {
        serde_json::Value::String(s) => Content::text(s),
        other => {
            let text =
                serde_json::to_string(&other).unwrap_or_else(|_| "<unserializable result>".into());
            Content::text(text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_impl() -> Implementation {
        Implementation::new("test-server", "0.0.1")
    }

    #[test]
    fn string_values_render_as_plain_text_not_quoted_json() {
        let content = value_to_content(json!("hello"));
        assert_eq!(content.as_text().map(|t| t.text.as_str()), Some("hello"));
    }

    #[test]
    fn structured_values_render_as_json() {
        let content = value_to_content(json!({"count": 7}));
        assert_eq!(
            content.as_text().map(|t| t.text.as_str()),
            Some(r#"{"count":7}"#),
        );
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

    // The tests below exercise the spec-mandated routing in
    // `package_tool_outcome` without scaffolding an rmcp `RequestContext`.
    // Together they pin the MCP `output_schema` contract: structured
    // tools route through `CallToolResult::structured`, unstructured
    // tools fall back to `Content::text`, and every `ToolError` variant
    // maps to the right JSON-RPC error code.

    #[test]
    fn structured_tool_with_object_result_routes_via_structured() {
        let result =
            package_tool_outcome("demo", true, Ok(json!({"answer": 42}))).expect("structured ok");
        // CallToolResult::structured sets BOTH a JSON-stringified text
        // fallback (for legacy clients) AND structured_content (for
        // spec-compliant clients). Assert both are present.
        assert_eq!(result.structured_content, Some(json!({"answer": 42})));
        assert_eq!(result.is_error, Some(false));
        assert_eq!(result.content.len(), 1);
    }

    #[test]
    fn structured_tool_with_non_object_result_is_internal_error() {
        let cases = [json!("hi"), json!([1, 2, 3]), json!(7), json!(null)];
        for value in cases {
            let err = package_tool_outcome("demo", true, Ok(value.clone())).unwrap_err();
            let payload = serde_json::to_value(&err).unwrap();
            assert_eq!(
                payload["code"], -32603,
                "{value} should map to internal error"
            );
            assert!(
                payload["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("output_schema"),
                "message must mention output_schema (got {})",
                payload["message"]
            );
        }
    }

    #[test]
    fn unstructured_tool_renders_string_as_plain_text() {
        let result =
            package_tool_outcome("demo", false, Ok(json!("hello"))).expect("unstructured ok");
        assert!(result.structured_content.is_none());
        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            result.content[0].as_text().map(|t| t.text.as_str()),
            Some("hello")
        );
    }

    #[test]
    fn unstructured_tool_renders_object_as_json_text() {
        let result =
            package_tool_outcome("demo", false, Ok(json!({"k": 1}))).expect("unstructured ok");
        assert!(result.structured_content.is_none());
        assert_eq!(
            result.content[0].as_text().map(|t| t.text.as_str()),
            Some(r#"{"k":1}"#),
        );
    }

    #[test]
    fn not_found_maps_to_invalid_params_passing_inner_message_through() {
        // Two distinct callers use NotFound:
        //   - Dispatcher when an unknown tool name is invoked
        //   - Tools themselves when their *resource* lookup misses
        // The adapter must not prepend a hard-coded "unknown tool:"
        // prefix, otherwise a `get_dataset(slug=nope)` failure reads
        // as "unknown tool: dataset not found" — nonsensical.
        for inner in [
            "tool `does_not_exist` is not registered",
            "dataset not found (slug=nope)",
        ] {
            let err = package_tool_outcome("demo", false, Err(ToolError::NotFound(inner.into())))
                .unwrap_err();
            let payload = serde_json::to_value(&err).unwrap();
            assert_eq!(payload["code"], -32602);
            assert_eq!(
                payload["message"].as_str().unwrap(),
                inner,
                "adapter must pass the NotFound message through unchanged",
            );
        }
    }

    #[test]
    fn invalid_arguments_maps_to_invalid_params() {
        let err = package_tool_outcome(
            "demo",
            false,
            Err(ToolError::InvalidArguments(
                "locale must be a string".into(),
            )),
        )
        .unwrap_err();
        let payload = serde_json::to_value(&err).unwrap();
        assert_eq!(payload["code"], -32602);
        assert!(payload["message"].as_str().unwrap().contains("locale"));
    }

    #[test]
    fn execution_failure_maps_to_internal_error() {
        let err = package_tool_outcome(
            "demo",
            false,
            Err(ToolError::Execution("downstream blew up".into())),
        )
        .unwrap_err();
        let payload = serde_json::to_value(&err).unwrap();
        assert_eq!(payload["code"], -32603);
        assert!(payload["message"].as_str().unwrap().contains("downstream"));
    }
}
