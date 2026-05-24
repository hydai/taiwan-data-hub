//! `geo_distance_haversine` MCP tool — great-circle distance between
//! two `(lat, lon)` points.
//!
//! Pure math, no I/O. Wraps [`crate::geo::distance_haversine_m`] in a
//! `ToolHandler` with strict input validation: both points must be
//! `(lat, lon)` numbers within valid earth-coordinate ranges, and the
//! response is always metres + kilometres so callers don't have to
//! think about unit conversion.

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};

use crate::geo::distance_haversine_m;
use crate::json_helpers::kind_of;

pub const TOOL_NAME: &str = "geo_distance_haversine";

#[derive(Debug, Default, Clone)]
pub struct DistanceHaversineTool;

impl DistanceHaversineTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for DistanceHaversineTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: "Great-circle (Haversine) distance between two \
                          (lat, lon) points on Earth. Returns metres and \
                          kilometres in the same response so callers don't \
                          have to convert. Assumes a spherical Earth — \
                          error stays well under 0.5 % for distances on \
                          the order of TW administrative boundaries."
                .to_string(),
            input_schema: input_schema(),
            output_schema: Some(output_schema()),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let (lat1, lon1) = parse_point(&args, "from")?;
        let (lat2, lon2) = parse_point(&args, "to")?;
        let metres = distance_haversine_m(lat1, lon1, lat2, lon2);
        Ok(json!({
            "metres": metres,
            "kilometres": metres / 1000.0,
        }))
    }
}

fn parse_point(args: &Value, key: &str) -> Result<(f64, f64), ToolError> {
    let obj = args.get(key).ok_or_else(|| {
        ToolError::InvalidArguments(format!("missing `{key}` (expected {{lat, lon}} object)"))
    })?;
    let map = obj.as_object().ok_or_else(|| {
        ToolError::InvalidArguments(format!(
            "`{key}` must be an object with lat / lon numbers, got {}",
            kind_of(obj)
        ))
    })?;
    let lat = parse_coord(map, key, "lat", -90.0, 90.0)?;
    let lon = parse_coord(map, key, "lon", -180.0, 180.0)?;
    Ok((lat, lon))
}

fn parse_coord(
    map: &Map<String, Value>,
    point_key: &str,
    coord_key: &str,
    min: f64,
    max: f64,
) -> Result<f64, ToolError> {
    let v = map.get(coord_key).ok_or_else(|| {
        ToolError::InvalidArguments(format!("`{point_key}.{coord_key}` is required"))
    })?;
    let n = v.as_f64().ok_or_else(|| {
        ToolError::InvalidArguments(format!(
            "`{point_key}.{coord_key}` must be a number, got {}",
            kind_of(v)
        ))
    })?;
    if !n.is_finite() || n < min || n > max {
        return Err(ToolError::InvalidArguments(format!(
            "`{point_key}.{coord_key}` must be a finite number in [{min}, {max}], got {n}"
        )));
    }
    Ok(n)
}

fn input_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["from", "to"],
        "properties": {
            "from": point_schema("starting"),
            "to": point_schema("ending"),
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled schema must be an object")
}

fn point_schema(label: &str) -> Value {
    json!({
        "type": "object",
        "required": ["lat", "lon"],
        "properties": {
            "lat": {"type": "number", "minimum": -90, "maximum": 90, "description": format!("{label} latitude in decimal degrees")},
            "lon": {"type": "number", "minimum": -180, "maximum": 180, "description": format!("{label} longitude in decimal degrees")},
        },
        "additionalProperties": false,
    })
}

fn output_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["metres", "kilometres"],
        "properties": {
            "metres": {"type": "number"},
            "kilometres": {"type": "number"},
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

    fn invoke(args: Value) -> Value {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(DistanceHaversineTool::new().call(args))
            .expect("call ok")
    }

    fn invoke_err(args: Value) -> ToolError {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(DistanceHaversineTool::new().call(args))
            .expect_err("call should error")
    }

    #[test]
    fn descriptor_advertises_schemas() {
        let d = DistanceHaversineTool::new().descriptor();
        assert_eq!(d.name, TOOL_NAME);
        assert!(d.output_schema.is_some());
    }

    #[test]
    fn happy_path_returns_metres_and_km() {
        let out = invoke(json!({
            "from": {"lat": 25.0337, "lon": 121.5645},
            "to":   {"lat": 25.0478, "lon": 121.5170},
        }));
        let m = out["metres"].as_f64().unwrap();
        let km = out["kilometres"].as_f64().unwrap();
        assert!((4_500.0..=5_100.0).contains(&m));
        assert!((m / 1000.0 - km).abs() < 1e-9);
    }

    #[test]
    fn missing_from_is_invalid_arguments() {
        let err = invoke_err(json!({"to": {"lat": 0.0, "lon": 0.0}}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn out_of_range_lat_is_invalid_arguments() {
        let err = invoke_err(json!({
            "from": {"lat": 95.0, "lon": 0.0},
            "to":   {"lat": 0.0, "lon": 0.0},
        }));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }
}
