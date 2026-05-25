//! MCP wrapper for the `json_schema_validate` tool (#6.10
//! follow-up). Thin facade over [`crate::json_schema::validate`].

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};

use crate::json_helpers::kind_of;
use crate::json_schema::{self, SchemaDraft};

pub const TOOL_NAME: &str = "json_schema_validate";

#[derive(Debug, Default, Clone)]
pub struct JsonSchemaValidateTool;
impl JsonSchemaValidateTool {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for JsonSchemaValidateTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: "Validate a JSON document against a JSON Schema. \
                Default draft is 2020-12; pass `draft` as \"7\", \"2019-09\", \
                or \"2020-12\" to select. Returns `{valid, errors}` where \
                `errors` lists every failure in document order with a JSON \
                Pointer into the data. A malformed schema (not a validation \
                failure) is surfaced as InvalidArguments — schema errors \
                are the caller's fault, not the document's."
                .to_string(),
            input_schema: input_schema(),
            output_schema: Some(
                json!({
                    "type": "object",
                    "required": ["valid", "errors"],
                    "properties": {
                        "valid": {
                            "type": "boolean",
                            "description": "True iff `errors` is empty. Mirrors \
                                the array length so clients can branch on a \
                                single bool without reading the array."
                        },
                        "errors": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "required": ["instance_path", "message"],
                                "properties": {
                                    "instance_path": {
                                        "type": "string",
                                        "description": "RFC 6901 JSON Pointer \
                                            into `data` (e.g. `/users/2/email`). \
                                            Empty when the root document failed."
                                    },
                                    "message": {
                                        "type": "string",
                                        "description": "Human-readable diagnostic \
                                            from the validator, naming the \
                                            failing keyword + the value that \
                                            tripped it."
                                    }
                                },
                                "additionalProperties": false
                            }
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
        let schema = args
            .get("schema")
            .cloned()
            .ok_or_else(|| ToolError::InvalidArguments("missing `schema`".into()))?;
        let draft = parse_draft(&args)?;

        let errors =
            json_schema::validate(&data, &schema, draft).map_err(ToolError::InvalidArguments)?;
        let valid = errors.is_empty();
        let errors_json: Vec<Value> = errors
            .into_iter()
            .map(|e| {
                json!({
                    "instance_path": e.instance_path,
                    "message": e.message,
                })
            })
            .collect();
        Ok(json!({ "valid": valid, "errors": errors_json }))
    }
}

fn parse_draft(args: &Value) -> Result<SchemaDraft, ToolError> {
    match args.get("draft") {
        None | Some(Value::Null) => Ok(SchemaDraft::default_draft()),
        Some(Value::String(s)) => SchemaDraft::parse(s).map_err(ToolError::InvalidArguments),
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`draft` must be a string when present, got {}",
            kind_of(other)
        ))),
    }
}

fn input_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["data", "schema"],
        "properties": {
            "data": {
                "description": "Document to validate. Any JSON value."
            },
            "schema": {
                "type": "object",
                "description": "JSON Schema to validate against. Must itself \
                    be a well-formed schema for the selected draft; a \
                    malformed schema returns InvalidArguments, not a \
                    validation failure."
            },
            "draft": {
                "type": "string",
                "enum": ["7", "2019-09", "2020-12",
                         "draft7", "draft-07",
                         "draft2019-09", "draft-2019-09",
                         "draft2020-12", "draft-2020-12"],
                "description": "Optional. Default is `2020-12`."
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

    fn user_schema() -> Value {
        json!({
            "type": "object",
            "required": ["name", "age"],
            "properties": {
                "name": { "type": "string", "minLength": 1 },
                "age":  { "type": "integer", "minimum": 0 }
            },
            "additionalProperties": false
        })
    }

    #[test]
    fn descriptor_required_fields_are_data_and_schema() {
        let d = JsonSchemaValidateTool::new().descriptor();
        assert_eq!(d.name, TOOL_NAME);
        let required = d.input_schema["required"].as_array().unwrap();
        let names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(names, vec!["data", "schema"]);
    }

    #[test]
    fn valid_document_returns_valid_true_and_no_errors() {
        let out = block_on(JsonSchemaValidateTool::new().call(json!({
            "data": { "name": "Alice", "age": 30 },
            "schema": user_schema()
        })))
        .expect("call");
        assert_eq!(out["valid"], true);
        assert_eq!(out["errors"], json!([]));
    }

    #[test]
    fn invalid_document_lists_every_failure() {
        let out = block_on(JsonSchemaValidateTool::new().call(json!({
            "data": { "name": 42, "age": -1, "spurious": "extra" },
            "schema": user_schema()
        })))
        .expect("call");
        assert_eq!(out["valid"], false);
        let errors = out["errors"].as_array().expect("errors array");
        // Three distinct failures: name type, age minimum, spurious key.
        assert!(
            errors.len() >= 3,
            "expected ≥ 3 errors, got {} ({errors:?})",
            errors.len(),
        );
        // Every entry carries both fields the schema promises.
        for err in errors {
            assert!(err["instance_path"].is_string());
            assert!(err["message"].is_string());
        }
    }

    #[test]
    fn explicit_draft_7_works_for_legacy_corpora() {
        let out = block_on(JsonSchemaValidateTool::new().call(json!({
            "data": "hello",
            "schema": { "type": "string", "minLength": 3 },
            "draft": "7"
        })))
        .expect("call");
        assert_eq!(out["valid"], true);
    }

    #[test]
    fn unknown_draft_value_is_invalid_arguments() {
        let err = block_on(JsonSchemaValidateTool::new().call(json!({
            "data": {},
            "schema": { "type": "object" },
            "draft": "bogus"
        })))
        .expect_err("must fail");
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn malformed_schema_is_invalid_arguments_not_a_validation_failure() {
        // `"type": "objet"` typo — would silently accept the
        // schema and validate every document as "passes" if we
        // collapsed compile errors into the errors list.
        let err = block_on(JsonSchemaValidateTool::new().call(json!({
            "data": {},
            "schema": { "type": "objet" }
        })))
        .expect_err("must fail");
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn missing_args_are_invalid_arguments() {
        let err = block_on(JsonSchemaValidateTool::new().call(json!({})))
            .expect_err("missing both");
        assert!(matches!(err, ToolError::InvalidArguments(_)));

        let err = block_on(JsonSchemaValidateTool::new().call(json!({
            "schema": { "type": "object" }
        })))
        .expect_err("missing data");
        assert!(matches!(err, ToolError::InvalidArguments(_)));

        let err = block_on(JsonSchemaValidateTool::new().call(json!({
            "data": {}
        })))
        .expect_err("missing schema");
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn null_draft_uses_default() {
        // Explicitly passing draft=null should be treated the
        // same as omitting it (default 2020-12). This matches the
        // JSON-arg parsing convention used elsewhere in the
        // crate so clients that round-trip via a JSON layer that
        // serialises `undefined → null` don't get a surprise.
        let out = block_on(JsonSchemaValidateTool::new().call(json!({
            "data": "fixed",
            "schema": { "const": "fixed" },
            "draft": null
        })))
        .expect("call");
        assert_eq!(out["valid"], true);
    }
}
