//! MCP wrappers for ID-generation tools: UUID v4, UUID v7, ULID.
//! All three produce a textual identifier; the differences are in
//! sortability and entropy source. See each tool's description for
//! when to pick which.

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::json_helpers::kind_of;

fn parse_optional_count(args: &Value) -> Result<usize, ToolError> {
    match args.get("count") {
        None | Some(Value::Null) => Ok(1),
        Some(Value::Number(n)) => {
            let v = n.as_u64().ok_or_else(|| {
                ToolError::InvalidArguments("`count` must be a positive integer".into())
            })?;
            if v == 0 {
                return Err(ToolError::InvalidArguments("`count` must be ≥ 1".into()));
            }
            if v > 1024 {
                return Err(ToolError::InvalidArguments(format!(
                    "`count` is {v}; maximum is 1024 to keep responses small"
                )));
            }
            #[allow(clippy::cast_possible_truncation)]
            Ok(v as usize)
        }
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`count` must be an integer, got {}",
            kind_of(other)
        ))),
    }
}

fn input_schema_with_count() -> Map<String, Value> {
    json!({
        "type": "object",
        "properties": {
            "count": {
                "type": "integer",
                "minimum": 1,
                "maximum": 1024,
                "default": 1,
                "description": "How many IDs to generate (1-1024)."
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
        "required": ["ids", "count"],
        "properties": {
            "ids": {"type": "array", "items": {"type": "string"}},
            "count": {"type": "integer"}
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled schema must be an object")
}

// =====================================================================
// generate_uuid_v4 — random
// =====================================================================

pub const TOOL_UUID_V4: &str = "generate_uuid_v4";

#[derive(Debug, Default, Clone)]
pub struct UuidV4Tool;
impl UuidV4Tool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for UuidV4Tool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_UUID_V4.to_string(),
            description: "Generate random UUIDv4 identifiers. Use when \
                          you need unique IDs with no inherent ordering \
                          — pick v7 instead if you want time-sortable \
                          IDs."
                .to_string(),
            input_schema: input_schema_with_count(),
            output_schema: Some(output_schema()),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let count = parse_optional_count(&args)?;
        let ids: Vec<String> = (0..count).map(|_| Uuid::new_v4().to_string()).collect();
        Ok(json!({"ids": ids, "count": count}))
    }
}

// =====================================================================
// generate_uuid_v7 — time-sortable
// =====================================================================

pub const TOOL_UUID_V7: &str = "generate_uuid_v7";

#[derive(Debug, Default, Clone)]
pub struct UuidV7Tool;
impl UuidV7Tool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for UuidV7Tool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_UUID_V7.to_string(),
            description: "Generate UUIDv7 identifiers. Same 36-character \
                          shape as v4 but the first 48 bits encode \
                          milliseconds since Unix epoch, so the IDs sort \
                          chronologically — ideal for primary keys / \
                          B-tree friendliness."
                .to_string(),
            input_schema: input_schema_with_count(),
            output_schema: Some(output_schema()),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let count = parse_optional_count(&args)?;
        let ids: Vec<String> = (0..count).map(|_| Uuid::now_v7().to_string()).collect();
        Ok(json!({"ids": ids, "count": count}))
    }
}

// =====================================================================
// generate_ulid — base32 time-sortable
// =====================================================================

pub const TOOL_ULID: &str = "generate_ulid";

#[derive(Debug, Default, Clone)]
pub struct UlidTool;
impl UlidTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for UlidTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_ULID.to_string(),
            description: "Generate ULID identifiers (26-char Crockford \
                          base-32). Time-sortable like UUIDv7 but using \
                          a shorter URL-safe alphabet — pick over v7 \
                          when display compactness matters and you don't \
                          need UUID interop."
                .to_string(),
            input_schema: input_schema_with_count(),
            output_schema: Some(output_schema()),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let count = parse_optional_count(&args)?;
        let ids: Vec<String> = (0..count).map(|_| ulid::Ulid::new().to_string()).collect();
        Ok(json!({"ids": ids, "count": count}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run<T: ToolHandler>(tool: &T, args: Value) -> Result<Value, ToolError> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(tool.call(args))
    }

    #[test]
    fn uuid_v4_default_count_is_one() {
        let out = run(&UuidV4Tool::new(), json!({})).unwrap();
        assert_eq!(out["count"], 1);
        let id = out["ids"][0].as_str().unwrap();
        assert!(Uuid::parse_str(id).is_ok());
    }

    #[test]
    fn uuid_v7_ids_share_recent_timestamp_prefix() {
        // `Uuid::now_v7()` does NOT guarantee monotonicity within a
        // single millisecond tick — the trailing 74 bits are random,
        // so two v7 IDs generated in the same ms can sort in any
        // order. We assert the time-encoded prefix only: each ID's
        // unix-ms timestamp should land within a reasonable window
        // around the test's wall clock.
        let out = run(&UuidV7Tool::new(), json!({"count": 5})).unwrap();
        // `as_millis()` returns u128; cast bound-checked: the
        // current ms-since-epoch comfortably fits in u64.
        #[allow(clippy::cast_possible_truncation)]
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        for v in out["ids"].as_array().unwrap() {
            let s = v.as_str().unwrap();
            let id = Uuid::parse_str(s).unwrap();
            let (ts_secs, ts_subsec_nanos) = id.get_timestamp().unwrap().to_unix();
            let id_ms = ts_secs * 1_000 + u64::from(ts_subsec_nanos) / 1_000_000;
            // ±60 s window — generous so a slow CI runner doesn't
            // flake; the real assertion is "the high 48 bits encode
            // a roughly-current timestamp".
            let diff = id_ms.abs_diff(now_ms);
            assert!(
                diff < 60_000,
                "v7 timestamp {id_ms} too far from now {now_ms}"
            );
        }
    }

    #[test]
    fn ulid_has_correct_shape() {
        let out = run(&UlidTool::new(), json!({"count": 3})).unwrap();
        for v in out["ids"].as_array().unwrap() {
            let s = v.as_str().unwrap();
            assert_eq!(s.len(), 26, "ULID must be 26 chars");
            assert!(ulid::Ulid::from_string(s).is_ok());
        }
    }

    #[test]
    fn count_capped() {
        let err = run(&UuidV4Tool::new(), json!({"count": 2000})).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn count_zero_rejected() {
        let err = run(&UlidTool::new(), json!({"count": 0})).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }
}
