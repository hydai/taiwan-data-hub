//! MCP-tool wrappers around [`crate::dictionaries`]. Five
//! categories × two tools (`get_by_id` + `search`) = 10 tools, all
//! built from one generic [`DictionaryTool`] that takes a
//! `&'static Dictionary` plus the wire metadata.
//!
//! Wire shape:
//!  - `get_by_id(code)`: returns the matched entry or
//!    `ToolError::NotFound`.
//!  - `search(query, limit?)`: returns an array of entries.
//!    `limit` defaults to 20, max 100.

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};

use crate::dictionaries::{
    ADMIN_DIVISIONS, BANK_CODES, COUNTY_CODES, DictEntry, Dictionary, MRT_STATIONS, POSTAL_CODES,
};

const DEFAULT_SEARCH_LIMIT: usize = 20;
const MAX_SEARCH_LIMIT: usize = 100;

#[derive(Debug, Clone, Copy)]
enum Mode {
    GetById,
    Search,
}

#[derive(Debug, Clone, Copy)]
pub struct DictionaryTool {
    tool_name: &'static str,
    mode: Mode,
    dictionary: &'static Dictionary,
}

#[async_trait]
impl ToolHandler for DictionaryTool {
    fn descriptor(&self) -> ToolDescriptor {
        let (description, input_schema, output_schema) = match self.mode {
            Mode::GetById => (
                format!(
                    "Look up a {} entry by exact code. Returns NotFound when the code isn't in the v0.1 baked table.",
                    self.dictionary.name,
                ),
                get_by_id_input_schema(),
                get_by_id_output_schema(),
            ),
            Mode::Search => (
                format!(
                    "Substring-search the {} table. Matches against name, aliases, and code-prefix; case-insensitive for ASCII. Returns up to `limit` entries (default {DEFAULT_SEARCH_LIMIT}, max {MAX_SEARCH_LIMIT}) in table order.",
                    self.dictionary.name,
                ),
                search_input_schema(),
                search_output_schema(),
            ),
        };
        ToolDescriptor {
            name: self.tool_name.to_string(),
            description,
            input_schema,
            output_schema: Some(output_schema),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let obj = args
            .as_object()
            .ok_or_else(|| ToolError::InvalidArguments("arguments must be a JSON object".into()))?;
        match self.mode {
            Mode::GetById => {
                let code = require_string(obj, "code")?;
                let entry = self.dictionary.get_by_id(&code).ok_or_else(|| {
                    ToolError::NotFound(format!(
                        "no {} entry with code `{code}`",
                        self.dictionary.name,
                    ))
                })?;
                Ok(render_entry(&entry))
            }
            Mode::Search => {
                let query = require_string(obj, "query")?;
                let limit = parse_limit(obj)?;
                let results = self.dictionary.search(&query, limit);
                Ok(json!({
                    "count": results.len(),
                    "results": results.iter().map(render_entry).collect::<Vec<_>>(),
                }))
            }
        }
    }
}

fn render_entry(e: &DictEntry) -> Value {
    json!({
        "code": e.code,
        "name": e.name,
        "aliases": e.aliases,
    })
}

fn require_string(obj: &Map<String, Value>, key: &str) -> Result<String, ToolError> {
    match obj.get(key) {
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Err(ToolError::InvalidArguments(format!(
                    "`{key}` must be a non-empty string"
                )))
            } else {
                Ok(trimmed.to_string())
            }
        }
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`{key}` must be a string, got {}",
            kind_of(other)
        ))),
        None => Err(ToolError::InvalidArguments(format!("missing `{key}`"))),
    }
}

fn parse_limit(obj: &Map<String, Value>) -> Result<usize, ToolError> {
    match obj.get("limit") {
        None | Some(Value::Null) => Ok(DEFAULT_SEARCH_LIMIT),
        Some(Value::Number(n)) => {
            let v = n.as_u64().ok_or_else(|| {
                ToolError::InvalidArguments(format!("`limit` must be a positive integer, got {n}"))
            })?;
            let v_usize = usize::try_from(v)
                .map_err(|_| ToolError::InvalidArguments("`limit` too large".into()))?;
            if v_usize == 0 {
                Err(ToolError::InvalidArguments("`limit` must be ≥ 1".into()))
            } else if v_usize > MAX_SEARCH_LIMIT {
                Err(ToolError::InvalidArguments(format!(
                    "`limit` must be ≤ {MAX_SEARCH_LIMIT}, got {v_usize}"
                )))
            } else {
                Ok(v_usize)
            }
        }
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`limit` must be an integer, got {}",
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

fn get_by_id_input_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["code"],
        "properties": {
            "code": { "type": "string", "minLength": 1, "pattern": "\\S", "description": "Exact code to look up (case-sensitive)." },
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled input schema must be an object")
}

fn search_input_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["query"],
        "properties": {
            "query": { "type": "string", "minLength": 1, "pattern": "\\S", "description": "Substring to search across name + aliases (case-insensitive for ASCII); also matches code-prefix." },
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_SEARCH_LIMIT,
                "default": DEFAULT_SEARCH_LIMIT,
                "description": "Maximum number of matches to return.",
            },
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled input schema must be an object")
}

fn entry_schema() -> Value {
    json!({
        "type": "object",
        "required": ["code", "name", "aliases"],
        "properties": {
            "code": { "type": "string" },
            "name": { "type": "string" },
            "aliases": { "type": "array", "items": { "type": "string" } },
        },
    })
}

fn get_by_id_output_schema() -> Map<String, Value> {
    entry_schema()
        .as_object()
        .cloned()
        .expect("entry schema is an object")
}

fn search_output_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["count", "results"],
        "properties": {
            "count": { "type": "integer", "minimum": 0 },
            "results": { "type": "array", "items": entry_schema() },
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled output schema must be an object")
}

// ============================================================
//  Per-category exports: tool name constants + handlers.
// ============================================================

macro_rules! dict_tools {
    (
        $(
            $get_tool:ident = $get_name:literal,
            $search_tool:ident = $search_name:literal,
            $get_const:ident,
            $search_const:ident,
            $dict:expr;
        )+
    ) => {
        $(
            pub const $get_const: &str = $get_name;
            pub const $search_const: &str = $search_name;

            pub static $get_tool: DictionaryTool = DictionaryTool {
                tool_name: $get_name,
                mode: Mode::GetById,
                dictionary: &$dict,
            };
            pub static $search_tool: DictionaryTool = DictionaryTool {
                tool_name: $search_name,
                mode: Mode::Search,
                dictionary: &$dict,
            };
        )+
    };
}

dict_tools! {
    ADMIN_DIVISION_GET_TOOL    = "tw_lookup_admin_code",    ADMIN_DIVISION_SEARCH_TOOL = "tw_search_admin_code",
        TOOL_ADMIN_LOOKUP,  TOOL_ADMIN_SEARCH,  ADMIN_DIVISIONS;
    MRT_STATION_GET_TOOL       = "tw_lookup_mrt_station",   MRT_STATION_SEARCH_TOOL    = "tw_search_mrt_station",
        TOOL_MRT_LOOKUP,    TOOL_MRT_SEARCH,    MRT_STATIONS;
    BANK_CODE_GET_TOOL         = "tw_lookup_bank_code",     BANK_CODE_SEARCH_TOOL      = "tw_search_bank_code",
        TOOL_BANK_LOOKUP,   TOOL_BANK_SEARCH,   BANK_CODES;
    POSTAL_CODE_GET_TOOL       = "tw_lookup_postal_code",   POSTAL_CODE_SEARCH_TOOL    = "tw_search_postal_code",
        TOOL_POSTAL_LOOKUP, TOOL_POSTAL_SEARCH, POSTAL_CODES;
    COUNTY_CODE_GET_TOOL       = "tw_lookup_county_code",   COUNTY_CODE_SEARCH_TOOL    = "tw_search_county_code",
        TOOL_COUNTY_LOOKUP, TOOL_COUNTY_SEARCH, COUNTY_CODES;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoke<T: ToolHandler>(tool: &T, args: Value) -> Value {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(tool.call(args)).expect("call ok")
    }

    fn invoke_err<T: ToolHandler>(tool: &T, args: Value) -> ToolError {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(tool.call(args)).expect_err("call should error")
    }

    #[test]
    fn admin_lookup_happy_path() {
        let out = invoke(&ADMIN_DIVISION_GET_TOOL, json!({"code": "63000070"}));
        assert_eq!(out["name"], "台北市信義區");
    }

    #[test]
    fn admin_lookup_unknown_returns_not_found() {
        let err = invoke_err(&ADMIN_DIVISION_GET_TOOL, json!({"code": "00000000"}));
        assert!(matches!(err, ToolError::NotFound(_)));
    }

    #[test]
    fn admin_search_substring() {
        let out = invoke(&ADMIN_DIVISION_SEARCH_TOOL, json!({"query": "信義"}));
        assert!(out["count"].as_u64().unwrap() >= 1);
        let results = out["results"].as_array().unwrap();
        assert!(results.iter().any(|r| r["code"] == "63000070"));
    }

    #[test]
    fn admin_search_respects_limit() {
        let out = invoke(
            &ADMIN_DIVISION_SEARCH_TOOL,
            json!({"query": "台北市", "limit": 3}),
        );
        assert!(out["count"].as_u64().unwrap() <= 3);
    }

    #[test]
    fn mrt_search_english_alias() {
        let out = invoke(&MRT_STATION_SEARCH_TOOL, json!({"query": "Taipei Main"}));
        assert!(out["count"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn bank_lookup_ctbc() {
        let out = invoke(&BANK_CODE_GET_TOOL, json!({"code": "822"}));
        assert_eq!(out["name"], "中國信託商業銀行");
    }

    #[test]
    fn bank_search_case_insensitive_alias() {
        let out = invoke(&BANK_CODE_SEARCH_TOOL, json!({"query": "esun"}));
        let results = out["results"].as_array().unwrap();
        assert!(results.iter().any(|r| r["code"] == "808"));
    }

    #[test]
    fn postal_lookup_xinyi() {
        let out = invoke(&POSTAL_CODE_GET_TOOL, json!({"code": "110"}));
        assert_eq!(out["name"], "台北市信義區");
    }

    #[test]
    fn county_search_pre_reorg_alias() {
        let out = invoke(&COUNTY_CODE_SEARCH_TOOL, json!({"query": "台北縣"}));
        let results = out["results"].as_array().unwrap();
        assert!(results.iter().any(|r| r["code"] == "ROC_CITY_NEW_TAIPEI"));
    }

    #[test]
    fn missing_code_rejected() {
        let err = invoke_err(&ADMIN_DIVISION_GET_TOOL, json!({}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn empty_query_rejected() {
        let err = invoke_err(&ADMIN_DIVISION_SEARCH_TOOL, json!({"query": ""}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn limit_above_max_rejected() {
        let err = invoke_err(
            &ADMIN_DIVISION_SEARCH_TOOL,
            json!({"query": "台北市", "limit": 1000}),
        );
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn limit_zero_rejected() {
        let err = invoke_err(
            &ADMIN_DIVISION_SEARCH_TOOL,
            json!({"query": "台北市", "limit": 0}),
        );
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn descriptors_advertise_schemas() {
        for tool in [
            &ADMIN_DIVISION_GET_TOOL,
            &ADMIN_DIVISION_SEARCH_TOOL,
            &MRT_STATION_GET_TOOL,
            &MRT_STATION_SEARCH_TOOL,
            &BANK_CODE_GET_TOOL,
            &BANK_CODE_SEARCH_TOOL,
            &POSTAL_CODE_GET_TOOL,
            &POSTAL_CODE_SEARCH_TOOL,
            &COUNTY_CODE_GET_TOOL,
            &COUNTY_CODE_SEARCH_TOOL,
        ] {
            let d = tool.descriptor();
            assert!(d.name.starts_with("tw_"), "got: {}", d.name);
            assert!(d.output_schema.is_some());
        }
    }
}
