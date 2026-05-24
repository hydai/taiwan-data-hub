//! `geo_reverse_geocode` MCP tool — coordinates → free-text + address
//! parts via OSM/Nominatim. Same rate-limit + UA discipline as
//! [`crate::geo_geocode_tool`].

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};

use crate::geo_nominatim::{self, NominatimError};
use crate::json_helpers::kind_of;

pub const TOOL_NAME: &str = "geo_reverse_geocode";

#[derive(Debug, Default, Clone)]
pub struct ReverseGeocodeTool;

impl ReverseGeocodeTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for ReverseGeocodeTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: "Reverse geocode (lat, lon) to a free-text \
                          display name and structured address parts \
                          via the OpenStreetMap Nominatim service. \
                          Same 1 req/s public-usage discipline as \
                          geo_geocode; set NOMINATIM_BASE_URL to point \
                          at a self-hosted Nominatim for higher \
                          throughput."
                .to_string(),
            input_schema: input_schema(),
            output_schema: Some(output_schema()),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let (lat, lon) = parse_coords(&args)?;
        match geo_nominatim::reverse(lat, lon).await {
            Ok(hit) => Ok(json!({
                "lat": lat,
                "lon": lon,
                "result": hit,
            })),
            Err(NominatimError::Status(s)) => {
                Err(ToolError::Execution(format!("Nominatim returned HTTP {s}")))
            }
            Err(e) => Err(ToolError::Execution(e.to_string())),
        }
    }
}

fn parse_coords(args: &Value) -> Result<(f64, f64), ToolError> {
    let lat = parse_finite(args, "lat", -90.0, 90.0)?;
    let lon = parse_finite(args, "lon", -180.0, 180.0)?;
    Ok((lat, lon))
}

fn parse_finite(args: &Value, key: &str, min: f64, max: f64) -> Result<f64, ToolError> {
    let v = args
        .get(key)
        .ok_or_else(|| ToolError::InvalidArguments(format!("`{key}` is required")))?;
    let n = v.as_f64().ok_or_else(|| {
        ToolError::InvalidArguments(format!("`{key}` must be a number, got {}", kind_of(v)))
    })?;
    if !n.is_finite() || n < min || n > max {
        return Err(ToolError::InvalidArguments(format!(
            "`{key}` must be a finite number in [{min}, {max}], got {n}"
        )));
    }
    Ok(n)
}

fn input_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["lat", "lon"],
        "properties": {
            "lat": {"type": "number", "minimum": -90, "maximum": 90},
            "lon": {"type": "number", "minimum": -180, "maximum": 180},
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
        "required": ["lat", "lon", "result"],
        "properties": {
            "lat": {"type": "number"},
            "lon": {"type": "number"},
            "result": {"type": "object"},
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
            .block_on(ReverseGeocodeTool::new().call(args))
            .expect_err("call should error")
    }

    #[test]
    fn descriptor_advertises_schemas() {
        let d = ReverseGeocodeTool::new().descriptor();
        assert_eq!(d.name, TOOL_NAME);
        assert!(d.output_schema.is_some());
    }

    #[test]
    fn missing_lat_is_invalid_arguments() {
        let err = invoke_err(json!({"lon": 121.5}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn out_of_range_lon_is_invalid_arguments() {
        let err = invoke_err(json!({"lat": 25.0, "lon": 200.0}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }
}
