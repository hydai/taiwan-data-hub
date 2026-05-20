//! `tw_validate_id` MCP tool — entry point for the four ID validators.
//!
//! One tool with `kind` dispatch keeps the agent surface small: the
//! LLM sees a single "validate a Taiwan ID" affordance instead of
//! having to pick the right validator first.
//!
//! `kind = "auto"` (the default) routes by the input's length+shape
//! signature, which is unambiguous because each format has a unique
//! envelope:
//! - 10 chars starting with a letter → `national_id`
//! - 9 digits                        → `passport`
//! - 8 digits                        → `tax_id`
//! - anything else                   → `unknown`
//!
//! The output shape per the issue's Definition of Done is `{valid,
//! kind, parsed}`. `parsed`
//! is `{}` when the input doesn't match any known shape; the per-
//! validator modules document their own `parsed` schemas.

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};

use crate::{national_id, passport, tax_id};

pub const TOOL_NAME: &str = "tw_validate_id";

/// User-facing `kind` values accepted on input. `arc` is an alias for
/// the modern unified resident-permit format (covered by
/// `national_id` since 2021). It's kept as a separate input value so
/// callers asking "is this an ARC?" can express that intent
/// explicitly; we still route it through `national_id::validate` and
/// surface the result narrowed to `kind == "resident"`.
const ACCEPTED_KINDS: &[&str] = &["auto", "national_id", "tax_id", "arc", "passport"];

#[derive(Debug, Default, Clone)]
pub struct ValidateIdTool;

impl ValidateIdTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for ValidateIdTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: "Validate a Taiwan identifier — national ID (身分證), \
                          unified business tax ID (統一編號), resident permit \
                          (居留證 / 統一證號), or ROC passport — and return \
                          structured metadata. Use kind=auto to detect by shape, \
                          or pass an explicit kind when the caller already knows \
                          which family the value belongs to."
                .to_string(),
            input_schema: input_schema(),
            output_schema: Some(output_schema()),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let value = parse_value(&args)?;
        let requested_kind = parse_kind(&args)?;

        let response = match requested_kind.as_str() {
            "auto" => dispatch_auto(&value),
            "national_id" => dispatch_national_id(&value),
            "arc" => dispatch_arc(&value),
            "tax_id" => dispatch_tax_id(&value),
            "passport" => dispatch_passport(&value),
            other => {
                return Err(ToolError::InvalidArguments(format!(
                    "`kind` must be one of {ACCEPTED_KINDS:?}, got {other:?}"
                )));
            }
        };
        Ok(response)
    }
}

fn parse_value(args: &Value) -> Result<String, ToolError> {
    match args.get("value") {
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        Some(Value::String(_)) => Err(ToolError::InvalidArguments(
            "`value` must be a non-empty string".into(),
        )),
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`value` must be a string, got {}",
            kind_of(other)
        ))),
        None => Err(ToolError::InvalidArguments("missing `value`".into())),
    }
}

fn parse_kind(args: &Value) -> Result<String, ToolError> {
    match args.get("kind") {
        None | Some(Value::Null) => Ok("auto".to_string()),
        Some(Value::String(s)) if s.is_empty() => Ok("auto".to_string()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`kind` must be a string, got {}",
            kind_of(other)
        ))),
    }
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Try each validator in length+shape order. Returns the first match.
/// When no shape recognizes the input, returns
/// `{valid: false, kind: "unknown", parsed: {}}`.
fn dispatch_auto(value: &str) -> Value {
    let trimmed = value.trim();
    // Branch by length+shape signature.
    if trimmed.len() == 10 {
        return dispatch_national_id(value);
    }
    if trimmed.len() == 9 && trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return dispatch_passport(value);
    }
    if trimmed.len() == 8 && trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return dispatch_tax_id(value);
    }
    json!({
        "valid": false,
        "kind": "unknown",
        "parsed": {},
    })
}

fn dispatch_national_id(value: &str) -> Value {
    let (valid, parsed) = national_id::validate(value);
    match parsed {
        Some(p) => {
            // `national_id::ParsedNationalId` already serializes to
            // the schema we advertise — embed it under `parsed`.
            json!({
                "valid": valid,
                "kind": p.kind.as_str(),
                "parsed": p,
            })
        }
        None => json!({
            "valid": false,
            "kind": "national_id",
            "parsed": {},
        }),
    }
}

/// `kind = "arc"` narrows to the resident-permit subset of the
/// `national_id` space. If the input parses as a citizen ID, return
/// `valid: false` with `kind: "arc"` so the caller's intent
/// ("validate as ARC") is honored — citizen IDs don't qualify as ARCs
/// even when they're well-formed.
fn dispatch_arc(value: &str) -> Value {
    let (valid, parsed) = national_id::validate(value);
    let Some(p) = parsed else {
        return json!({"valid": false, "kind": "arc", "parsed": {}});
    };
    match p.kind {
        national_id::NationalIdKind::Resident => json!({
            "valid": valid,
            "kind": "arc",
            "parsed": p,
        }),
        national_id::NationalIdKind::LegacyResident => json!({
            // Legacy 2-letter shape is recognized but unverified
            // (see national_id module docs). Still flag the legacy
            // kind so callers see "this looks like a legacy ARC".
            "valid": false,
            "kind": "legacy_resident",
            "parsed": p,
        }),
        national_id::NationalIdKind::Citizen => json!({
            // Caller asked for ARC, parse landed on citizen — report
            // the caller's intent.
            "valid": false,
            "kind": "arc",
            "parsed": {},
        }),
    }
}

fn dispatch_tax_id(value: &str) -> Value {
    let (valid, parsed) = tax_id::validate(value);
    match parsed {
        Some(p) => json!({
            "valid": valid,
            "kind": "tax_id",
            "parsed": p,
        }),
        None => json!({
            "valid": false,
            "kind": "tax_id",
            "parsed": {},
        }),
    }
}

fn dispatch_passport(value: &str) -> Value {
    let (valid, parsed) = passport::validate(value);
    match parsed {
        Some(p) => json!({
            "valid": valid,
            "kind": "passport",
            "parsed": p,
        }),
        None => json!({
            "valid": false,
            "kind": "passport",
            "parsed": {},
        }),
    }
}

fn input_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["value"],
        "properties": {
            "value": {
                "type": "string",
                "minLength": 1,
                "description": "The identifier to validate. ASCII letters are \
                                 normalized to upper case; surrounding whitespace \
                                 is stripped."
            },
            "kind": {
                "type": "string",
                "enum": ["auto", "national_id", "tax_id", "arc", "passport"],
                "default": "auto",
                "description": "Identifier family. `auto` (default) routes by \
                                 length+shape signature; explicit kinds force a \
                                 single validator and return valid=false when the \
                                 input doesn't match that family."
            }
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled input schema must be an object")
}

fn output_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["valid", "kind", "parsed"],
        "properties": {
            "valid": { "type": "boolean" },
            "kind": {
                "type": "string",
                "enum": [
                    "citizen",
                    "resident",
                    "legacy_resident",
                    "tax_id",
                    "passport",
                    "arc",
                    "national_id",
                    "unknown",
                ],
                "description": "Detected (or requested) ID family. The values \
                                 `citizen`, `resident`, and `legacy_resident` \
                                 appear only when auto-detection picked the \
                                 national_id validator and could narrow further; \
                                 explicit-kind requests echo the caller's value."
            },
            "parsed": {
                "type": "object",
                "description": "Family-specific structured fields. Empty when \
                                 the input didn't match any known shape."
            }
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled output schema must be an object")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoke(args: Value) -> Value {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(ValidateIdTool::new().call(args))
            .expect("call ok")
    }

    fn invoke_err(args: Value) -> ToolError {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(ValidateIdTool::new().call(args))
            .expect_err("call should error")
    }

    #[test]
    fn descriptor_advertises_input_and_output_schemas() {
        let d = ValidateIdTool::new().descriptor();
        assert_eq!(d.name, "tw_validate_id");
        assert!(d.output_schema.is_some());
    }

    #[test]
    fn missing_value_is_invalid_arguments() {
        let err = invoke_err(json!({}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn non_string_value_is_invalid_arguments() {
        let err = invoke_err(json!({"value": 42}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn unknown_kind_is_invalid_arguments() {
        let err = invoke_err(json!({"value": "A123456789", "kind": "passport2"}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn auto_routes_national_id_by_length() {
        let out = invoke(json!({"value": "A123456789"}));
        assert_eq!(out["valid"], true);
        assert_eq!(out["kind"], "citizen");
        assert_eq!(out["parsed"]["canonical"], "A123456789");
    }

    #[test]
    fn auto_routes_tax_id_by_length() {
        let out = invoke(json!({"value": "12345675"}));
        assert_eq!(out["valid"], true);
        assert_eq!(out["kind"], "tax_id");
        assert_eq!(out["parsed"]["canonical"], "12345675");
    }

    #[test]
    fn auto_routes_passport_by_length() {
        let out = invoke(json!({"value": "123456789"}));
        assert_eq!(out["valid"], true);
        assert_eq!(out["kind"], "passport");
        assert_eq!(out["parsed"]["canonical"], "123456789");
    }

    #[test]
    fn auto_unknown_for_garbled_input() {
        let out = invoke(json!({"value": "hello"}));
        assert_eq!(out["valid"], false);
        assert_eq!(out["kind"], "unknown");
        assert_eq!(out["parsed"], json!({}));
    }

    #[test]
    fn explicit_arc_rejects_citizen_id() {
        // A123456789 is a valid citizen ID — but asking "is this an
        // ARC?" must return false.
        let out = invoke(json!({"value": "A123456789", "kind": "arc"}));
        assert_eq!(out["valid"], false);
        assert_eq!(out["kind"], "arc");
    }

    #[test]
    fn explicit_arc_accepts_modern_resident_id() {
        // Computed in national_id::tests: A812345671 valid modern resident.
        let out = invoke(json!({"value": "A812345671", "kind": "arc"}));
        assert_eq!(out["valid"], true);
        assert_eq!(out["kind"], "arc");
    }

    #[test]
    fn explicit_tax_id_returns_legacy_alternative_flag() {
        // 00000078 — legacy +1 path (per tax_id::tests).
        let out = invoke(json!({"value": "00000078", "kind": "tax_id"}));
        assert_eq!(out["valid"], true);
        assert_eq!(out["kind"], "tax_id");
        assert_eq!(out["parsed"]["strict_2023"], false);
        assert_eq!(out["parsed"]["legacy_alternative"], true);
    }

    #[test]
    fn checksum_failure_keeps_valid_false_and_returns_parsed_metadata() {
        let out = invoke(json!({"value": "A123456788"}));
        assert_eq!(out["valid"], false);
        // Auto-routing recognized it as a national_id shape and
        // narrowed to citizen, even though the check digit failed.
        assert_eq!(out["kind"], "citizen");
        assert_eq!(out["parsed"]["canonical"], "A123456788");
    }

    #[test]
    fn explicit_national_id_passes_through_kind_narrowing() {
        let out = invoke(json!({"value": "A123456789", "kind": "national_id"}));
        assert_eq!(out["valid"], true);
        // national_id narrows further on auto-detection — here we
        // surface the narrowed value (citizen) rather than the broad
        // umbrella kind, so callers always see the most specific
        // family for valid inputs.
        assert_eq!(out["kind"], "citizen");
    }

    #[test]
    fn empty_value_is_invalid_arguments() {
        let err = invoke_err(json!({"value": ""}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn whitespace_only_value_returns_unknown_via_auto() {
        // Non-empty per parse_value, but auto-dispatch trims and
        // finds no length match.
        let out = invoke(json!({"value": "   "}));
        assert_eq!(out["valid"], false);
        assert_eq!(out["kind"], "unknown");
    }

    #[test]
    fn output_conforms_to_declared_schema_shape() {
        // Cheap structural check (same pattern as list_domains).
        for value in ["A123456789", "12345675", "123456789", "garbled"] {
            let out = invoke(json!({"value": value}));
            assert!(out["valid"].is_boolean(), "{value}");
            assert!(out["kind"].is_string(), "{value}");
            assert!(out["parsed"].is_object(), "{value}");
        }
    }
}
