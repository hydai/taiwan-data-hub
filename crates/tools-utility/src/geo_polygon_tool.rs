//! `geo_point_in_polygon` MCP tool — ray-casting point-in-polygon test
//! on a planar lat/lon polygon.

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};

use crate::geo::point_in_polygon;
use crate::json_helpers::kind_of;

pub const TOOL_NAME: &str = "geo_point_in_polygon";

/// Maximum vertex count accepted by the MCP wrapper. Matches the
/// 100 k cap the stats tools use for their `values` arrays — the
/// ray-cast loop is O(n) and we want a hard upper bound so an
/// adversarial caller can't push the dispatcher into seconds of
/// Vec allocation. Reinforced both in the JSON schema
/// (`maxItems`) and at parse time in case a future caller bypasses
/// schema validation.
const MAX_POLYGON_VERTICES: usize = 100_000;

#[derive(Debug, Default, Clone)]
pub struct PointInPolygonTool;

impl PointInPolygonTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for PointInPolygonTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: "Test whether a (lat, lon) point lies inside a \
                          polygon supplied as an ordered list of \
                          {lat, lon} vertices. Uses planar ray-casting \
                          — suitable for polygons whose extent is small \
                          relative to Earth's curvature (TW townships, \
                          city boundaries). Polygon may be open or \
                          closed; the loop wraps around implicitly. \
                          Returns false for degenerate polygons \
                          (< 3 vertices). On-boundary points are NOT \
                          handled specially — the ray-cast decision \
                          is sensitive to floating-point thresholds, \
                          so callers needing on-boundary detection \
                          should test for that separately."
                .to_string(),
            input_schema: input_schema(),
            output_schema: Some(output_schema()),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let (lat, lon) = parse_point(&args)?;
        let vertices = parse_vertices(&args)?;
        let inside = point_in_polygon(lat, lon, &vertices);
        Ok(json!({"inside": inside, "vertex_count": vertices.len()}))
    }
}

fn parse_point(args: &Value) -> Result<(f64, f64), ToolError> {
    let obj = args.get("point").ok_or_else(|| {
        ToolError::InvalidArguments("missing `point` (expected {lat, lon})".into())
    })?;
    let map = obj.as_object().ok_or_else(|| {
        ToolError::InvalidArguments(format!("`point` must be an object, got {}", kind_of(obj)))
    })?;
    let lat = parse_finite(map, "point", "lat", -90.0, 90.0)?;
    let lon = parse_finite(map, "point", "lon", -180.0, 180.0)?;
    Ok((lat, lon))
}

fn parse_vertices(args: &Value) -> Result<Vec<(f64, f64)>, ToolError> {
    let arr = args
        .get("polygon")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ToolError::InvalidArguments("`polygon` must be an array of {lat, lon} vertices".into())
        })?;
    // Defence-in-depth vertex cap: the JSON schema also declares
    // `maxItems`, but enforcing the bound at parse time means
    // direct Rust callers (or any path that skips schema
    // validation) still get a structured error instead of an
    // OOM / multi-second allocation.
    if arr.len() > MAX_POLYGON_VERTICES {
        return Err(ToolError::InvalidArguments(format!(
            "`polygon` has {} vertices; maximum is {MAX_POLYGON_VERTICES}",
            arr.len()
        )));
    }
    let mut out = Vec::with_capacity(arr.len());
    for (idx, v) in arr.iter().enumerate() {
        let map = v.as_object().ok_or_else(|| {
            ToolError::InvalidArguments(format!(
                "`polygon[{idx}]` must be an object with lat/lon, got {}",
                kind_of(v)
            ))
        })?;
        let label = format!("polygon[{idx}]");
        let lat = parse_finite(map, &label, "lat", -90.0, 90.0)?;
        let lon = parse_finite(map, &label, "lon", -180.0, 180.0)?;
        out.push((lat, lon));
    }
    Ok(out)
}

fn parse_finite(
    map: &Map<String, Value>,
    parent: &str,
    key: &str,
    min: f64,
    max: f64,
) -> Result<f64, ToolError> {
    let v = map
        .get(key)
        .ok_or_else(|| ToolError::InvalidArguments(format!("`{parent}.{key}` is required")))?;
    let n = v.as_f64().ok_or_else(|| {
        ToolError::InvalidArguments(format!(
            "`{parent}.{key}` must be a number, got {}",
            kind_of(v)
        ))
    })?;
    if !n.is_finite() || n < min || n > max {
        return Err(ToolError::InvalidArguments(format!(
            "`{parent}.{key}` must be a finite number in [{min}, {max}], got {n}"
        )));
    }
    Ok(n)
}

fn input_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["point", "polygon"],
        "properties": {
            "point": {
                "type": "object",
                "required": ["lat", "lon"],
                "properties": {
                    "lat": {"type": "number", "minimum": -90, "maximum": 90},
                    "lon": {"type": "number", "minimum": -180, "maximum": 180},
                },
                "additionalProperties": false,
            },
            "polygon": {
                "type": "array",
                "maxItems": MAX_POLYGON_VERTICES,
                "items": {
                    "type": "object",
                    "required": ["lat", "lon"],
                    "properties": {
                        "lat": {"type": "number", "minimum": -90, "maximum": 90},
                        "lon": {"type": "number", "minimum": -180, "maximum": 180},
                    },
                    "additionalProperties": false,
                },
                "description": "Ordered vertex list. May be open or closed."
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
        "required": ["inside", "vertex_count"],
        "properties": {
            "inside": {"type": "boolean"},
            "vertex_count": {"type": "integer", "minimum": 0},
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
            .block_on(PointInPolygonTool::new().call(args))
            .expect("call ok")
    }

    #[test]
    fn inside_unit_square() {
        let out = invoke(json!({
            "point": {"lat": 0.5, "lon": 0.5},
            "polygon": [
                {"lat": 0.0, "lon": 0.0},
                {"lat": 0.0, "lon": 1.0},
                {"lat": 1.0, "lon": 1.0},
                {"lat": 1.0, "lon": 0.0},
            ],
        }));
        assert_eq!(out["inside"], true);
        assert_eq!(out["vertex_count"], 4);
    }

    #[test]
    fn outside_unit_square() {
        let out = invoke(json!({
            "point": {"lat": 2.0, "lon": 2.0},
            "polygon": [
                {"lat": 0.0, "lon": 0.0},
                {"lat": 0.0, "lon": 1.0},
                {"lat": 1.0, "lon": 1.0},
                {"lat": 1.0, "lon": 0.0},
            ],
        }));
        assert_eq!(out["inside"], false);
    }

    #[test]
    fn degenerate_polygon_returns_false() {
        let out = invoke(json!({
            "point": {"lat": 0.0, "lon": 0.0},
            "polygon": [{"lat": 0.0, "lon": 0.0}],
        }));
        assert_eq!(out["inside"], false);
        assert_eq!(out["vertex_count"], 1);
    }
}
