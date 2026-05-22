//! `tw_validate_format` MCP tool — single entrypoint for the 8
//! wave-1 format validators in [`crate::formats`]. Same dispatch
//! shape as `tw_validate_id`: one `kind` discriminator + one
//! `value` payload, return `{valid, kind, detail}`.

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};

use crate::formats::{FormatKind, validate};
use crate::json_helpers::kind_of;

pub const TOOL_NAME: &str = "tw_validate_format";

#[derive(Debug, Default, Clone)]
pub struct ValidateFormatTool;

#[async_trait]
impl ToolHandler for ValidateFormatTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: "Validate a Taiwan wave-1 format: invoice (統一發票), \
                          taipower (台電電號), water_meter (自來水水號), \
                          phone (中華電信市話/手機), license_plate (車牌 4/6/7 字), \
                          credit_card (LUHN), iban (ISO 13616 mod-97), or \
                          iata_airport (3-letter code). Returns {valid, kind, detail}; \
                          detail is the resolved airport name on iata_airport, the \
                          issuer hint on a valid credit_card, and null otherwise."
                .to_string(),
            input_schema: input_schema(),
            output_schema: Some(output_schema()),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let obj = args
            .as_object()
            .ok_or_else(|| ToolError::InvalidArguments("arguments must be a JSON object".into()))?;
        let kind = parse_kind(obj)?;
        let value = parse_value(obj)?;
        let result = validate(kind, &value);
        Ok(json!({
            "valid": result.valid,
            "kind": result.kind.as_str(),
            "detail": result.detail,
        }))
    }
}

fn parse_kind(obj: &Map<String, Value>) -> Result<FormatKind, ToolError> {
    let value = obj
        .get("kind")
        .ok_or_else(|| ToolError::InvalidArguments("missing `kind`".into()))?;
    let s = match value {
        Value::String(s) => s.trim(),
        other => {
            return Err(ToolError::InvalidArguments(format!(
                "`kind` must be a string, got {}",
                kind_of(other)
            )));
        }
    };
    FormatKind::from_wire(s).ok_or_else(|| {
        let accepted: Vec<String> = FormatKind::ALL
            .iter()
            .map(|k| format!("\"{}\"", k.as_str()))
            .collect();
        ToolError::InvalidArguments(format!(
            "`kind` must be one of {}, got {s:?}",
            accepted.join(", "),
        ))
    })
}

fn parse_value(obj: &Map<String, Value>) -> Result<String, ToolError> {
    match obj.get("value") {
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Err(ToolError::InvalidArguments(
                    "`value` must be a non-empty string".into(),
                ))
            } else {
                Ok(trimmed.to_string())
            }
        }
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`value` must be a string, got {}",
            kind_of(other)
        ))),
        None => Err(ToolError::InvalidArguments("missing `value`".into())),
    }
}

fn input_schema() -> Map<String, Value> {
    let kinds: Vec<&'static str> = FormatKind::ALL.iter().map(|k| k.as_str()).collect();
    json!({
        "type": "object",
        "required": ["kind", "value"],
        "properties": {
            "kind": {
                "type": "string",
                "enum": kinds,
                "description": "Which format to validate.",
            },
            "value": {
                "type": "string",
                "minLength": 1,
                "pattern": "\\S",
                "description": "The candidate value to validate. Surrounding whitespace is stripped; format-specific separators (spaces, hyphens) are accepted where natural.",
            },
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled input schema must be an object")
}

fn output_schema() -> Map<String, Value> {
    let kinds: Vec<&'static str> = FormatKind::ALL.iter().map(|k| k.as_str()).collect();
    json!({
        "type": "object",
        "required": ["valid", "kind", "detail"],
        "properties": {
            "valid": { "type": "boolean" },
            "kind": { "type": "string", "enum": kinds },
            "detail": { "type": ["string", "null"], "description": "Resolved airport name (iata_airport), issuer hint (credit_card when valid), or null otherwise." },
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
        rt.block_on(ValidateFormatTool.call(args)).expect("call ok")
    }

    fn invoke_err(args: Value) -> ToolError {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(ValidateFormatTool.call(args))
            .expect_err("call should error")
    }

    #[test]
    fn descriptor_advertises_schemas() {
        let d = ValidateFormatTool.descriptor();
        assert_eq!(d.name, "tw_validate_format");
        assert!(d.output_schema.is_some());
    }

    #[test]
    fn credit_card_valid_visa() {
        let out = invoke(json!({"kind": "credit_card", "value": "4111111111111111"}));
        assert_eq!(out["valid"], true);
        assert_eq!(out["kind"], "credit_card");
        assert_eq!(out["detail"], "Visa");
    }

    #[test]
    fn credit_card_invalid_luhn() {
        let out = invoke(json!({"kind": "credit_card", "value": "4111111111111112"}));
        assert_eq!(out["valid"], false);
        assert!(out["detail"].is_null());
    }

    #[test]
    fn iban_valid_iso_example() {
        let out = invoke(json!({"kind": "iban", "value": "GB82 WEST 1234 5698 7654 32"}));
        assert_eq!(out["valid"], true);
    }

    #[test]
    fn iata_lookup_tpe() {
        let out = invoke(json!({"kind": "iata_airport", "value": "TPE"}));
        assert_eq!(out["valid"], true);
        assert_eq!(out["detail"], "Taipei Taoyuan International");
    }

    #[test]
    fn iata_lookup_unknown() {
        let out = invoke(json!({"kind": "iata_airport", "value": "ZZZ"}));
        assert_eq!(out["valid"], false);
        assert!(out["detail"].is_null());
    }

    #[test]
    fn phone_mobile() {
        let out = invoke(json!({"kind": "phone", "value": "0912-345-678"}));
        assert_eq!(out["valid"], true);
    }

    #[test]
    fn license_plate_modern() {
        let out = invoke(json!({"kind": "license_plate", "value": "ABC-1234"}));
        assert_eq!(out["valid"], true);
    }

    #[test]
    fn unknown_kind_rejected() {
        let err = invoke_err(json!({"kind": "blockchain", "value": "0xdeadbeef"}));
        match err {
            ToolError::InvalidArguments(m) => assert!(m.contains("blockchain"), "got: {m}"),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[test]
    fn missing_kind_rejected() {
        let err = invoke_err(json!({"value": "TPE"}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn missing_value_rejected() {
        let err = invoke_err(json!({"kind": "iata_airport"}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn empty_value_rejected() {
        let err = invoke_err(json!({"kind": "iata_airport", "value": "   "}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn non_string_kind_rejected() {
        let err = invoke_err(json!({"kind": 42, "value": "TPE"}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }
}
