//! `geo_geocode` MCP tool — forward geocode (free-text → lat/lon) via
//! OSM/Nominatim. See `geo_nominatim.rs` for the rate-limit + UA
//! discipline this tool delegates to.

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};

use crate::geo_nominatim::{self, NominatimError};
use crate::json_helpers::kind_of;

pub const TOOL_NAME: &str = "geo_geocode";

const DEFAULT_LIMIT: u32 = 5;
const MAX_LIMIT: u32 = 10;

#[derive(Debug, Default, Clone)]
pub struct GeocodeTool;

impl GeocodeTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for GeocodeTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: "Forward geocode a free-text address or place \
                          name to coordinates via the OpenStreetMap \
                          Nominatim service. Respects Nominatim's 1 req/s \
                          public-usage policy via a process-wide \
                          throttle. For higher throughput, self-host \
                          Nominatim and set NOMINATIM_BASE_URL."
                .to_string(),
            input_schema: input_schema(),
            output_schema: Some(output_schema()),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let query = parse_query(&args)?;
        let limit = parse_limit(&args)?;
        match geo_nominatim::search(&query, limit).await {
            Ok(hits) => Ok(json!({
                "query": query,
                "results": hits,
            })),
            Err(NominatimError::Status(s)) => {
                Err(ToolError::Execution(format!("Nominatim returned HTTP {s}")))
            }
            Err(e) => Err(ToolError::Execution(e.to_string())),
        }
    }
}

fn parse_query(args: &Value) -> Result<String, ToolError> {
    match args.get("query") {
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                Err(ToolError::InvalidArguments(
                    "`query` must be a non-empty string after trimming".into(),
                ))
            } else {
                Ok(t.to_string())
            }
        }
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`query` must be a string, got {}",
            kind_of(other)
        ))),
        None => Err(ToolError::InvalidArguments("missing `query`".into())),
    }
}

fn parse_limit(args: &Value) -> Result<u32, ToolError> {
    match args.get("limit") {
        None | Some(Value::Null) => Ok(DEFAULT_LIMIT),
        Some(Value::Number(n)) => {
            let v = n.as_u64().ok_or_else(|| {
                ToolError::InvalidArguments("`limit` must be a positive integer".into())
            })?;
            if v == 0 || v > u64::from(MAX_LIMIT) {
                return Err(ToolError::InvalidArguments(format!(
                    "`limit` must be in 1..={MAX_LIMIT}, got {v}"
                )));
            }
            // Bound-checked above (v ≤ MAX_LIMIT = 10), so the u32
            // cast is lossless. Allow the truncation lint at the
            // call site since the surrounding logic is the proof.
            #[allow(clippy::cast_possible_truncation)]
            Ok(v as u32)
        }
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`limit` must be an integer, got {}",
            kind_of(other)
        ))),
    }
}

fn input_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["query"],
        "properties": {
            "query": {
                "type": "string",
                "minLength": 1,
                "description": "Free-text address or place name (e.g. `台北 101`)."
            },
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_LIMIT,
                "default": DEFAULT_LIMIT,
                "description": "Maximum results to return (default 5, max 10)."
            }
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled schema must be an object")
}

fn output_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["query", "results"],
        "properties": {
            "query": {"type": "string"},
            "results": {
                "type": "array",
                "items": {"type": "object"}
            }
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled schema must be an object")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoke_err(args: Value) -> ToolError {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(GeocodeTool::new().call(args))
            .expect_err("call should error")
    }

    #[test]
    fn descriptor_advertises_schemas() {
        let d = GeocodeTool::new().descriptor();
        assert_eq!(d.name, TOOL_NAME);
        assert!(d.output_schema.is_some());
    }

    #[test]
    fn missing_query_is_invalid_arguments() {
        let err = invoke_err(json!({}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn whitespace_query_is_invalid_arguments() {
        let err = invoke_err(json!({"query": "   "}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn out_of_range_limit_is_invalid_arguments() {
        let err = invoke_err(json!({"query": "Taipei", "limit": 99}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    /// We deliberately do NOT hit Nominatim from unit tests — the
    /// service rate-limits aggressively and would slow CI. Successful
    /// round-trips are exercised manually + via the integration
    /// playground.
    #[test]
    fn limit_zero_is_invalid_arguments() {
        let err = invoke_err(json!({"query": "Taipei", "limit": 0}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }
}
