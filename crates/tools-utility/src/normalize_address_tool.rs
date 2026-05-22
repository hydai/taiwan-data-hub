//! `tw_normalize_address` MCP tool — surfaces
//! [`crate::address::normalize_address`] to MCP clients.
//!
//! The tool is intentionally permissive about the *content* of the
//! address — pure-junk strings (Latin text, garbled CJK) don't
//! error; they just return a struct of `None`s with
//! `normalized: false`. The MCP argument shape is still validated,
//! though: a missing, non-string, empty, or whitespace-only
//! `address` surfaces as `ToolError::InvalidArguments` so callers
//! can fix the call shape distinct from "the address didn't parse".
//!
//! The response is `{ parts: {...}, normalized: bool }` where
//! `parts` is the structured form with every field optional, and
//! `normalized` is `true` when at least `county` and `district`
//! were parsed (the conservative "this looks like a Taiwan
//! address" signal — agents can rely on it to decide whether to
//! trust the breakdown).

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};

use crate::address::normalize_address;
use crate::json_helpers::kind_of;

pub const TOOL_NAME: &str = "tw_normalize_address";

#[derive(Debug, Default, Clone)]
pub struct NormalizeAddressTool;

impl NormalizeAddressTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for NormalizeAddressTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: "Split a free-form Taiwan address into county, district, road, \
                          section, lane, alley, number, and floor. 改制 county names are \
                          normalised to their post-restructuring forms \
                          (台中縣→台中市, 高雄縣→高雄市, etc.). Returns `normalized: false` \
                          when the input doesn't look like a Taiwan address (no county + \
                          district found)."
                .to_string(),
            input_schema: input_schema(),
            output_schema: Some(output_schema()),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let address = parse_address(&args)?;
        let parts = normalize_address(&address);
        let normalized = parts.county.is_some() && parts.district.is_some();
        Ok(json!({
            "parts": parts,
            "normalized": normalized,
        }))
    }
}

fn parse_address(args: &Value) -> Result<String, ToolError> {
    match args.get("address") {
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Err(ToolError::InvalidArguments(
                    "`address` must be a non-empty string (after trimming whitespace)".into(),
                ))
            } else {
                Ok(trimmed.to_string())
            }
        }
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`address` must be a string, got {}",
            kind_of(other)
        ))),
        None => Err(ToolError::InvalidArguments("missing `address`".into())),
    }
}

fn input_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["address"],
        "properties": {
            "address": {
                "type": "string",
                "minLength": 1,
                "description": "Free-form Taiwan address. Must contain at least one non-whitespace character (whitespace-only strings are rejected after trimming). ASCII / 全形 whitespace and commas are stripped before parsing.",
            },
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled input schema must be an object")
}

fn output_schema() -> Map<String, Value> {
    let part_field = |description: &str| {
        json!({
            "type": ["string", "null"],
            "description": description,
        })
    };
    json!({
        "type": "object",
        "required": ["parts", "normalized"],
        "properties": {
            "parts": {
                "type": "object",
                "required": ["county", "district", "road", "section", "lane", "alley", "number", "floor"],
                "properties": {
                    "county": part_field("縣 / 市 in post-改制 canonical form (e.g. \"台北市\", \"新北市\"). Includes the suffix character."),
                    "district": part_field("鄉 / 鎮 / 市 / 區. Includes the suffix character (e.g. \"信義區\")."),
                    "road": part_field("路 / 街 / 道 with the trailing suffix character (e.g. \"忠孝東路\")."),
                    "section": part_field("Numeric portion only (no `段` suffix): Arabic (\"2\") or Chinese (\"二\") numeral."),
                    "lane": part_field("Numeric portion only (no `巷` suffix)."),
                    "alley": part_field("Numeric portion only (no `弄` suffix)."),
                    "number": part_field("Numeric portion only (no `號` suffix). Supports `123`, `123-1`, `123之5`."),
                    "floor": part_field("Numeric portion only (no `樓` / `F` suffix). Examples: `5`, `B1`."),
                },
                "additionalProperties": false,
            },
            "normalized": {
                "type": "boolean",
                "description": "True iff both county and district parsed — the conservative \"this looks like a Taiwan address\" signal.",
            },
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
        rt.block_on(NormalizeAddressTool::new().call(args))
            .expect("call ok")
    }

    fn invoke_err(args: Value) -> ToolError {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(NormalizeAddressTool::new().call(args))
            .expect_err("call should error")
    }

    #[test]
    fn descriptor_advertises_input_and_output_schemas() {
        let d = NormalizeAddressTool::new().descriptor();
        assert_eq!(d.name, "tw_normalize_address");
        assert!(d.output_schema.is_some());
    }

    #[test]
    fn happy_path_normalizes_and_flags_true() {
        let out = invoke(json!({"address": "台北市信義區市府路45號5樓"}));
        assert_eq!(out["normalized"], true);
        assert_eq!(out["parts"]["county"], "台北市");
        assert_eq!(out["parts"]["district"], "信義區");
        assert_eq!(out["parts"]["road"], "市府路");
        assert_eq!(out["parts"]["number"], "45");
        assert_eq!(out["parts"]["floor"], "5");
    }

    #[test]
    fn pre_reorg_taichung_county_normalises() {
        let out = invoke(json!({"address": "台中縣豐原區中正路100號"}));
        assert_eq!(out["normalized"], true);
        assert_eq!(out["parts"]["county"], "台中市");
        assert_eq!(out["parts"]["district"], "豐原區");
    }

    #[test]
    fn junk_input_flags_false_but_returns_struct() {
        let out = invoke(json!({"address": "hello world"}));
        assert_eq!(out["normalized"], false);
        // parts is still present, with every field null.
        assert!(out["parts"]["county"].is_null());
        assert!(out["parts"]["district"].is_null());
    }

    #[test]
    fn missing_address_is_invalid_arguments() {
        let err = invoke_err(json!({}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn empty_address_is_invalid_arguments() {
        let err = invoke_err(json!({"address": ""}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn whitespace_only_address_is_invalid_arguments() {
        let err = invoke_err(json!({"address": "   "}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn non_string_address_is_invalid_arguments() {
        let err = invoke_err(json!({"address": 42}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn full_address_field_round_trip() {
        let out = invoke(json!({"address": "台北市大安區忠孝東路四段153巷5弄12號3樓"}));
        assert_eq!(out["normalized"], true);
        assert_eq!(out["parts"]["road"], "忠孝東路");
        assert_eq!(out["parts"]["section"], "四");
        assert_eq!(out["parts"]["lane"], "153");
        assert_eq!(out["parts"]["alley"], "5");
        assert_eq!(out["parts"]["number"], "12");
        assert_eq!(out["parts"]["floor"], "3");
    }
}
