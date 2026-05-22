//! `tw_canonical_city_district` MCP tool — surfaces
//! [`crate::canonical::canonicalize`] over MCP.
//!
//! Takes `county` (required) and `district` (optional), returns
//! the canonical `CountyCode` + `DistrictCode` together with the
//! canonical zh-TW county name (post-改制 form) and the raw
//! district input for caller fallback.

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};

use crate::canonical::{Canonical, CountyCode, DistrictCode, canonicalize};

pub const TOOL_NAME: &str = "tw_canonical_city_district";

#[derive(Debug, Default, Clone)]
pub struct CanonicalCityDistrictTool;

#[async_trait]
impl ToolHandler for CanonicalCityDistrictTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: "Canonicalize a TW county / district pair to stable identifier \
                          codes. Accepts post-改制 names (e.g. 新北市/板橋區), pre-改制 \
                          aliases (e.g. 台北縣 → 新北市), and traditional 臺 forms. \
                          v0.1 bakes district codes for the 6 直轄市 (台北 / 新北 / 桃園 \
                          / 台中 / 台南 / 高雄) — other counties resolve the county \
                          code while `district_code` returns null (with the raw \
                          input preserved in `district_raw` for caller fallback)."
                .to_string(),
            input_schema: input_schema(),
            output_schema: Some(output_schema()),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let (county, district) = parse_args(&args)?;
        let out = canonicalize(&county, district.as_deref());
        Ok(render(&out))
    }
}

fn render(c: &Canonical) -> Value {
    let district_code = match &c.district_code {
        DistrictCode::Known(code) => Value::String((*code).to_string()),
        DistrictCode::Unknown => Value::Null,
    };
    json!({
        "county_code": c.county_code.map(CountyCode::as_code),
        "county_name_zh": c.county_name_zh,
        "district_code": district_code,
        "district_raw": c.district_raw,
    })
}

fn parse_args(args: &Value) -> Result<(String, Option<String>), ToolError> {
    let obj = args
        .as_object()
        .ok_or_else(|| ToolError::InvalidArguments("arguments must be a JSON object".into()))?;
    let county = match obj.get("county") {
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Err(ToolError::InvalidArguments(
                    "`county` must be a non-empty string".into(),
                ));
            }
            trimmed.to_string()
        }
        Some(other) => {
            return Err(ToolError::InvalidArguments(format!(
                "`county` must be a string, got {}",
                kind_of(other)
            )));
        }
        None => {
            return Err(ToolError::InvalidArguments(
                "missing `county` (required)".into(),
            ));
        }
    };
    let district = match obj.get("district") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Some(other) => {
            return Err(ToolError::InvalidArguments(format!(
                "`district` must be a string when provided, got {}",
                kind_of(other)
            )));
        }
    };
    Ok((county, district))
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

fn input_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["county"],
        "properties": {
            "county": {
                "type": "string",
                "minLength": 1,
                // \\S+ requires at least one non-whitespace char so
                // schema validation matches the runtime trim+empty
                // check.
                "pattern": "\\S",
                "description": "Free-form county name (post-改制, pre-改制 alias, or 臺 form). Must contain at least one non-whitespace character. Examples: \"台北市\", \"新北市\", \"台中縣\", \"臺北縣\".",
            },
            "district": {
                "type": ["string", "null"],
                "minLength": 1,
                "description": "Optional district name (e.g. \"信義區\", \"板橋區\"). Pass null or omit the field to get a county-only canonicalisation. Empty/whitespace strings are treated as null.",
            },
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
        "required": ["county_code", "county_name_zh", "district_code", "district_raw"],
        "properties": {
            "county_code": {
                "type": ["string", "null"],
                "description": "Stable county identifier (e.g. \"ROC_CITY_NEW_TAIPEI\"). Null when the input county couldn't be resolved.",
            },
            "county_name_zh": {
                "type": ["string", "null"],
                "description": "Canonical zh-TW county name in post-改制 form.",
            },
            "district_code": {
                "type": ["string", "null"],
                "description": "Stable district identifier (e.g. \"DIST_TPE_XINYI\"). Null when no district was provided, or when the district couldn't be resolved.",
            },
            "district_raw": {
                "type": ["string", "null"],
                "description": "Raw district input echoed back with ASCII + 全形 whitespace and commas stripped (including *internal* whitespace, so \"信義 區\" → \"信義區\"). Provided for caller fallback when district_code is null.",
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
        rt.block_on(CanonicalCityDistrictTool.call(args))
            .expect("call ok")
    }

    fn invoke_err(args: Value) -> ToolError {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(CanonicalCityDistrictTool.call(args))
            .expect_err("call should error")
    }

    #[test]
    fn descriptor_advertises_input_and_output_schemas() {
        let d = CanonicalCityDistrictTool.descriptor();
        assert_eq!(d.name, "tw_canonical_city_district");
        assert!(d.output_schema.is_some());
    }

    #[test]
    fn taipei_xinyi_happy_path() {
        let out = invoke(json!({"county": "台北市", "district": "信義區"}));
        assert_eq!(out["county_code"], "ROC_CITY_TAIPEI");
        assert_eq!(out["county_name_zh"], "台北市");
        assert_eq!(out["district_code"], "DIST_TPE_XINYI");
        assert_eq!(out["district_raw"], "信義區");
    }

    #[test]
    fn pre_reorg_taipei_county_normalises_to_new_taipei() {
        let out = invoke(json!({"county": "台北縣", "district": "板橋市"}));
        assert_eq!(out["county_code"], "ROC_CITY_NEW_TAIPEI");
        assert_eq!(out["county_name_zh"], "新北市");
        // 板橋市 (pre-改制) → 板橋區.
        assert_eq!(out["district_code"], "DIST_NTPE_BANQIAO");
    }

    #[test]
    fn county_only_returns_null_district() {
        let out = invoke(json!({"county": "台北市"}));
        assert_eq!(out["county_code"], "ROC_CITY_TAIPEI");
        assert!(out["district_code"].is_null());
        assert!(out["district_raw"].is_null());
    }

    #[test]
    fn non_municipality_returns_county_but_null_district() {
        // 新竹縣 is not one of the 6 直轄市 v0.1 districts table.
        let out = invoke(json!({"county": "新竹縣", "district": "竹北市"}));
        assert_eq!(out["county_code"], "ROC_COUNTY_HSINCHU");
        assert!(out["district_code"].is_null());
        // Raw input preserved so caller can fall back.
        assert_eq!(out["district_raw"], "竹北市");
    }

    #[test]
    fn unknown_county_resolves_to_null() {
        let out = invoke(json!({"county": "Atlantis"}));
        assert!(out["county_code"].is_null());
        assert!(out["county_name_zh"].is_null());
    }

    #[test]
    fn missing_county_rejected() {
        let err = invoke_err(json!({}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn empty_county_rejected() {
        let err = invoke_err(json!({"county": ""}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn non_string_county_rejected() {
        let err = invoke_err(json!({"county": 42}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn non_string_district_rejected() {
        let err = invoke_err(json!({"county": "台北市", "district": 42}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }
}
