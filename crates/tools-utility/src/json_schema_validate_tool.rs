//! MCP wrapper for the `json_schema_validate` tool (#6.10
//! follow-up). Thin facade over [`crate::json_schema::validate`].

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};

use crate::json_helpers::kind_of;
use crate::json_schema::{self, SchemaDraft};

pub const TOOL_NAME: &str = "json_schema_validate";

/// Cap on serialised UTF-8 byte count of `data` and `schema`
/// (independent caps; each must fit). 4 MiB matches the rest of
/// the crate's body-size contract; a document or schema an
/// order of magnitude larger should be paginated upstream.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Hard cap on the validation-error list. A pathological schema
/// (say `"not": true`) can yield as many errors as the document
/// has leaves; capping keeps the response bounded and prevents a
/// runaway error walk from chewing the async runtime. The agent
/// only needs to see "enough" errors to know the document is
/// invalid; the rest are noise.
const MAX_ERRORS: usize = 200;

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
                    "required": ["valid", "errors", "truncated"],
                    "properties": {
                        "valid": {
                            "type": "boolean",
                            "description": "True iff `errors` is empty AND \
                                `truncated` is false. Mirrors the array \
                                length so clients can branch on a single \
                                bool without reading the array."
                        },
                        "errors": {
                            "type": "array",
                            "maxItems": 200,
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
                        },
                        "truncated": {
                            "type": "boolean",
                            "description": "True when the validator produced \
                                more than 200 errors and only the first 200 \
                                are returned. `valid` is also false in that \
                                case — the document didn't pass."
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
        check_body_size(&data, "data")?;
        let schema = args
            .get("schema")
            .cloned()
            .ok_or_else(|| ToolError::InvalidArguments("missing `schema`".into()))?;
        check_body_size(&schema, "schema")?;
        let draft = parse_draft(&args)?;

        // `validate` short-circuits at MAX_ERRORS so the validator
        // stops walking the schema as soon as the cap is hit —
        // bounds runtime + memory, not just response size.
        let (errors, truncated) = json_schema::validate(&data, &schema, draft, MAX_ERRORS)
            .map_err(ToolError::InvalidArguments)?;
        let valid = errors.is_empty() && !truncated;
        let errors_json: Vec<Value> = errors
            .into_iter()
            .map(|e| {
                json!({
                    "instance_path": e.instance_path,
                    "message": e.message,
                })
            })
            .collect();
        Ok(json!({
            "valid": valid,
            "errors": errors_json,
            "truncated": truncated,
        }))
    }
}

/// Reject `data` / `schema` values whose serialised form exceeds
/// [`MAX_BODY_BYTES`]. Serialised independently for each field
/// so a single oversized one can be named in the error.
fn check_body_size(value: &Value, field: &str) -> Result<(), ToolError> {
    let bytes = serde_json::to_vec(value).map_err(|e| {
        ToolError::InvalidArguments(format!("could not measure `{field}` size: {e}"))
    })?;
    if bytes.len() > MAX_BODY_BYTES {
        return Err(ToolError::InvalidArguments(format!(
            "`{field}` is {} bytes; maximum is {MAX_BODY_BYTES}",
            bytes.len()
        )));
    }
    Ok(())
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
                // JSON Schema spec lets the top-level schema be a
                // boolean: `true` accepts everything, `false`
                // rejects everything. The `jsonschema` crate
                // honours that, so the wrapper must too — declaring
                // `type: object` here would have advertised a
                // narrower contract than the underlying validator
                // actually accepts.
                "type": ["object", "boolean"],
                "description": "JSON Schema to validate against. May be an \
                    object schema OR a boolean (`true` = accept anything, \
                    `false` = reject everything — per the JSON Schema \
                    spec). Must itself be well-formed for the selected \
                    draft; a malformed schema returns InvalidArguments, \
                    not a validation failure."
            },
            "draft": {
                // `["string", "null"]` so a client that serialises
                // `undefined → null` (e.g. JS layers between agent
                // and gateway) gets the documented default-fallback
                // behaviour rather than a schema-layer rejection.
                // The runtime handler treats `null` and "absent" as
                // synonymous; pinning that here keeps the contract
                // and the implementation in step.
                "type": ["string", "null"],
                "enum": ["7", "2019-09", "2020-12",
                         "draft7", "draft-07",
                         "draft2019-09", "draft-2019-09",
                         "draft2020-12", "draft-2020-12",
                         null],
                "description": "Optional. Default (also when sent as `null`) is `2020-12`."
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
        let err =
            block_on(JsonSchemaValidateTool::new().call(json!({}))).expect_err("missing both");
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
    fn happy_path_reports_truncated_false() {
        let out = block_on(JsonSchemaValidateTool::new().call(json!({
            "data": { "name": "Alice", "age": 30 },
            "schema": user_schema()
        })))
        .expect("call");
        assert_eq!(out["truncated"], false);
    }

    #[test]
    fn over_max_errors_truncates_and_flags() {
        // `{"not": {}}` matches everything (since {} accepts anything),
        // so `not {}` rejects everything; for an array of N items
        // requiring `not` matching at every position, the validator
        // emits one error per element. Build an array of MAX_ERRORS+1
        // elements to overflow the cap.
        let big: Vec<i32> = (0..i32::try_from(MAX_ERRORS + 50).unwrap()).collect();
        let schema = json!({
            "type": "array",
            "items": { "not": {} }
        });
        let out = block_on(JsonSchemaValidateTool::new().call(json!({
            "data": big,
            "schema": schema
        })))
        .expect("call");
        let errors = out["errors"].as_array().expect("errors");
        assert_eq!(
            errors.len(),
            MAX_ERRORS,
            "must truncate to exactly MAX_ERRORS"
        );
        assert_eq!(out["truncated"], true);
        assert_eq!(
            out["valid"], false,
            "truncated implies validation failed; `valid` is false even though `errors` was capped",
        );
    }

    #[test]
    fn oversize_data_is_rejected_before_validation() {
        let huge = "x".repeat(MAX_BODY_BYTES);
        let err = block_on(JsonSchemaValidateTool::new().call(json!({
            "data": [huge],
            "schema": { "type": "array" }
        })))
        .expect_err("must fail");
        match err {
            ToolError::InvalidArguments(msg) => assert!(
                msg.contains("data") && msg.contains("maximum"),
                "expected `data` size-cap message, got {msg:?}",
            ),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[test]
    fn oversize_schema_is_rejected_before_validation() {
        // Pad the schema's description until it exceeds the body
        // cap. The actual schema content is valid; only the size
        // matters for this check.
        let huge_desc = "x".repeat(MAX_BODY_BYTES);
        let err = block_on(JsonSchemaValidateTool::new().call(json!({
            "data": {},
            "schema": { "type": "object", "description": huge_desc }
        })))
        .expect_err("must fail");
        match err {
            ToolError::InvalidArguments(msg) => assert!(
                msg.contains("schema") && msg.contains("maximum"),
                "expected `schema` size-cap message, got {msg:?}",
            ),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[test]
    fn boolean_true_schema_accepts_anything() {
        // Per the JSON Schema spec, `true` at the top level is
        // the always-accept schema. The tool wrapper must let
        // these through — Copilot R1 caught that the original
        // input_schema forced `schema` to be an object, which
        // would have rejected this perfectly valid request at
        // the rmcp layer before reaching the validator.
        for doc in [
            json!(null),
            json!(42),
            json!("hello"),
            json!({"a": 1}),
            json!([1, 2, 3]),
        ] {
            let out = block_on(JsonSchemaValidateTool::new().call(json!({
                "data": doc,
                "schema": true
            })))
            .expect("call");
            assert_eq!(out["valid"], true);
            assert_eq!(out["errors"], json!([]));
        }
    }

    #[test]
    fn boolean_false_schema_rejects_everything() {
        let out = block_on(JsonSchemaValidateTool::new().call(json!({
            "data": { "any": "value" },
            "schema": false
        })))
        .expect("call");
        assert_eq!(out["valid"], false);
        let errors = out["errors"].as_array().expect("errors array");
        assert_eq!(errors.len(), 1, "false-schema → exactly one rejection");
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
