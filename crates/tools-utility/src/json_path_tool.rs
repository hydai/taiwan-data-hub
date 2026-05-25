//! MCP wrapper for the `json_path` tool (#6.10 follow-up).
//!
//! Thin facade over [`crate::json_path::query`] — argument
//! parsing, schema, and the response envelope only. Pure logic
//! lives in the parent module so unit tests don't need to spin
//! up rmcp and Rust callers don't pay for `serde_json::Value`
//! round-trips.

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};

use crate::json_helpers::kind_of;
use crate::json_path;

pub const TOOL_NAME: &str = "json_path";

/// Schema cap on the expression text. `JSONPath` expressions are
/// human-authored; even the longest filter we'd realistically see
/// fits in a single screen of text. A hostile expression an order
/// of magnitude larger would chew parser time without benefit; cap
/// it at the wrapper boundary so we never reach the parser.
const MAX_EXPRESSION_BYTES: usize = 4 * 1024;

#[derive(Debug, Default, Clone)]
pub struct JsonPathTool;
impl JsonPathTool {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for JsonPathTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: "Apply an RFC 9535 `JSONPath` expression to a JSON \
                value and return every matching node in document order. \
                Empty result is reported as `{matches: [], count: 0}` — a \
                well-formed expression that selects nothing is NOT an \
                error. Malformed expressions (parser errors) are surfaced \
                as InvalidArguments."
                .to_string(),
            input_schema: input_schema(),
            output_schema: Some(
                json!({
                    "type": "object",
                    "required": ["matches", "count"],
                    "properties": {
                        "matches": {
                            "type": "array",
                            "description": "Matched JSON nodes in document order.",
                            "items": {}
                        },
                        "count": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "Length of `matches` — provided so \
                                clients can pre-allocate / short-circuit on \
                                empty results without reading the array."
                        }
                    },
                    "additionalProperties": false
                })
                .as_object()
                .cloned()
                .expect("hand-rolled schema must be an object"),
            ),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let data = args
            .get("data")
            .cloned()
            .ok_or_else(|| ToolError::InvalidArguments("missing `data`".into()))?;
        let expression = parse_expression(&args)?;
        let matches = json_path::query(&data, &expression).map_err(ToolError::InvalidArguments)?;
        let count = matches.len();
        Ok(json!({ "matches": matches, "count": count }))
    }
}

fn parse_expression(args: &Value) -> Result<String, ToolError> {
    match args.get("expression") {
        Some(Value::String(s)) => {
            if s.len() > MAX_EXPRESSION_BYTES {
                Err(ToolError::InvalidArguments(format!(
                    "`expression` is {} bytes; maximum is {MAX_EXPRESSION_BYTES}",
                    s.len()
                )))
            } else if s.is_empty() {
                Err(ToolError::InvalidArguments(
                    "`expression` must not be empty".into(),
                ))
            } else {
                Ok(s.clone())
            }
        }
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`expression` must be a string, got {}",
            kind_of(other)
        ))),
        None => Err(ToolError::InvalidArguments("missing `expression`".into())),
    }
}

fn input_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["data", "expression"],
        "properties": {
            "data": {
                "description": "Any JSON value to query."
            },
            "expression": {
                "type": "string",
                "minLength": 1,
                "description": "RFC 9535 `JSONPath` expression (e.g. \
                    `$.store.book[?@.price > 10].title`). Server caps at \
                    4 KiB."
            }
        },
        "additionalProperties": false
    })
    .as_object()
    .cloned()
    .expect("hand-rolled schema must be an object")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(future)
    }

    #[test]
    fn descriptor_advertises_name_and_required_fields() {
        let d = JsonPathTool::new().descriptor();
        assert_eq!(d.name, TOOL_NAME);
        let required = d.input_schema["required"].as_array().unwrap();
        let names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(names.contains(&"data"));
        assert!(names.contains(&"expression"));
    }

    #[test]
    fn happy_path_returns_matches_and_count() {
        let result = block_on(JsonPathTool::new().call(json!({
            "data": { "items": [{"v": 1}, {"v": 2}, {"v": 3}] },
            "expression": "$.items[*].v"
        })))
        .expect("query");
        assert_eq!(result["count"], 3);
        assert_eq!(result["matches"], json!([1, 2, 3]));
    }

    #[test]
    fn no_match_is_success_with_empty_array() {
        // The contract: a valid expression that selects nothing
        // returns success, NOT InvalidArguments. The matches list
        // is empty and count is 0 so clients can branch cleanly.
        let result = block_on(JsonPathTool::new().call(json!({
            "data": { "a": 1 },
            "expression": "$.does.not.exist"
        })))
        .expect("query");
        assert_eq!(result["count"], 0);
        assert_eq!(result["matches"], json!([]));
    }

    #[test]
    fn malformed_expression_surfaces_invalid_arguments() {
        let err = block_on(JsonPathTool::new().call(json!({
            "data": {},
            "expression": "$.[bad syntax"
        })))
        .expect_err("must fail");
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn missing_or_wrong_type_args_are_invalid_arguments() {
        let err = block_on(JsonPathTool::new().call(json!({}))).expect_err("missing both");
        assert!(matches!(err, ToolError::InvalidArguments(_)));

        let err = block_on(JsonPathTool::new().call(json!({"expression": "$"})))
            .expect_err("missing data");
        assert!(matches!(err, ToolError::InvalidArguments(_)));

        let err = block_on(JsonPathTool::new().call(json!({
            "data": {},
            "expression": 42
        })))
        .expect_err("wrong-type expression");
        assert!(matches!(err, ToolError::InvalidArguments(_)));

        let err = block_on(JsonPathTool::new().call(json!({
            "data": {},
            "expression": ""
        })))
        .expect_err("empty expression");
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn oversize_expression_is_rejected_before_parsing() {
        let huge = "$".repeat(MAX_EXPRESSION_BYTES + 1);
        let err = block_on(JsonPathTool::new().call(json!({
            "data": {},
            "expression": huge
        })))
        .expect_err("must fail");
        match err {
            ToolError::InvalidArguments(msg) => assert!(
                msg.contains("expression") && msg.contains("maximum"),
                "expected size cap message, got {msg:?}",
            ),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }
}
