//! Transport-agnostic MCP tool dispatcher.
//!
//! `mcp-core` owns the protocol abstraction so that rmcp (or any future MCP
//! SDK) is just a transport adapter. Concrete tools implement [`ToolHandler`]
//! using only `serde_json::Value` for arguments and results — they never
//! touch rmcp types.
//!
//! The runtime registry is [`Dispatcher`]; build one with
//! [`Dispatcher::builder`] and register handlers. Once built, the dispatcher
//! is cheap to clone (`Arc` inside).

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

/// Metadata about a tool exposed to MCP clients.
///
/// `input_schema` is a JSON Schema object — the outer wrapping must be
/// `{"type": "object", ...}` per the MCP spec.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: Map<String, Value>,
}

/// Errors a [`ToolHandler`] (or the dispatcher) can return.
#[derive(Debug, Error)]
pub enum ToolError {
    #[error("tool not found: {0}")]
    NotFound(String),
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
    #[error("execution failed: {0}")]
    Execution(String),
}

/// A single MCP tool implementation.
///
/// Implementations are registered with a [`DispatcherBuilder`] and invoked
/// by name. The argument and result types are `serde_json::Value` so that
/// tools never depend on the transport SDK.
#[async_trait]
pub trait ToolHandler: Send + Sync + 'static {
    fn descriptor(&self) -> ToolDescriptor;
    async fn call(&self, args: Value) -> Result<Value, ToolError>;
}

/// Immutable registry of [`ToolHandler`]s, keyed by tool name.
///
/// Cheap to clone (the inner map is wrapped in `Arc`). Construct via
/// [`Dispatcher::builder`].
#[derive(Clone, Default)]
pub struct Dispatcher {
    tools: Arc<BTreeMap<String, Arc<dyn ToolHandler>>>,
}

impl Dispatcher {
    pub fn builder() -> DispatcherBuilder {
        DispatcherBuilder::default()
    }

    pub fn list_tools(&self) -> Vec<ToolDescriptor> {
        self.tools.values().map(|t| t.descriptor()).collect()
    }

    pub async fn call_tool(&self, name: &str, args: Value) -> Result<Value, ToolError> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| ToolError::NotFound(name.to_string()))?;
        tool.call(args).await
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }
}

impl std::fmt::Debug for Dispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dispatcher")
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Builder for [`Dispatcher`]. Duplicate tool names overwrite earlier
/// registrations — the builder logs at WARN level when this happens.
#[derive(Default)]
pub struct DispatcherBuilder {
    tools: BTreeMap<String, Arc<dyn ToolHandler>>,
}

impl DispatcherBuilder {
    pub fn register<T: ToolHandler>(mut self, tool: T) -> Self {
        let name = tool.descriptor().name;
        if self.tools.contains_key(&name) {
            tracing::warn!(tool = %name, "duplicate tool registration; overwriting");
        }
        self.tools.insert(name, Arc::new(tool));
        self
    }

    pub fn build(self) -> Dispatcher {
        Dispatcher {
            tools: Arc::new(self.tools),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct EchoTool;

    #[async_trait]
    impl ToolHandler for EchoTool {
        fn descriptor(&self) -> ToolDescriptor {
            ToolDescriptor {
                name: "echo".into(),
                description: "Echoes its `message` argument back.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {"message": {"type": "string"}},
                    "required": ["message"],
                })
                .as_object()
                .cloned()
                .unwrap(),
            }
        }

        async fn call(&self, args: Value) -> Result<Value, ToolError> {
            let msg = args
                .get("message")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::InvalidArguments("missing `message`".into()))?;
            Ok(json!({"echo": msg}))
        }
    }

    #[tokio::test]
    async fn empty_dispatcher_lists_no_tools() {
        let d = Dispatcher::default();
        assert!(d.is_empty());
        assert_eq!(d.list_tools(), Vec::new());
    }

    #[tokio::test]
    async fn call_unknown_tool_returns_not_found() {
        let d = Dispatcher::default();
        let err = d.call_tool("missing", Value::Null).await.unwrap_err();
        assert!(matches!(err, ToolError::NotFound(name) if name == "missing"));
    }

    #[tokio::test]
    async fn registered_tool_round_trips() {
        let d = Dispatcher::builder().register(EchoTool).build();
        assert_eq!(d.len(), 1);
        assert!(d.contains("echo"));

        let descriptors = d.list_tools();
        assert_eq!(descriptors.len(), 1);
        assert_eq!(descriptors[0].name, "echo");

        let out = d.call_tool("echo", json!({"message": "hi"})).await.unwrap();
        assert_eq!(out, json!({"echo": "hi"}));
    }

    #[tokio::test]
    async fn invalid_args_propagate() {
        let d = Dispatcher::builder().register(EchoTool).build();
        let err = d.call_tool("echo", json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }
}
