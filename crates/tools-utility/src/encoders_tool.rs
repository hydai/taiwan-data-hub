//! MCP wrappers for the four encoder tools: base64 / url / hex /
//! `jwt_decode`. Each is a thin facade over a pure function in
//! [`crate::encoders`].

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};

use crate::encoders;
use crate::json_helpers::kind_of;

const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;

fn parse_input(args: &Value, key: &str) -> Result<String, ToolError> {
    match args.get(key) {
        Some(Value::String(s)) => {
            if s.len() > MAX_INPUT_BYTES {
                Err(ToolError::InvalidArguments(format!(
                    "`{key}` is {} bytes; maximum is {MAX_INPUT_BYTES}",
                    s.len()
                )))
            } else {
                Ok(s.clone())
            }
        }
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`{key}` must be a string, got {}",
            kind_of(other)
        ))),
        None => Err(ToolError::InvalidArguments(format!("missing `{key}`"))),
    }
}

fn parse_optional_bool(args: &Value, key: &str, default: bool) -> Result<bool, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`{key}` must be a boolean, got {}",
            kind_of(other)
        ))),
    }
}

fn input_schema_with_text() -> Map<String, Value> {
    // No `maxLength` constraint in the schema: JSON Schema's
    // `maxLength` counts Unicode code points but our runtime cap
    // uses `String::len()` (UTF-8 bytes). For multi-byte input
    // the two disagree, so we'd risk telling clients "valid" via
    // the schema and then erroring server-side (or vice versa).
    // Document the byte cap in the description instead and rely
    // on the runtime check in `parse_input`.
    json!({
        "type": "object",
        "required": ["text"],
        "properties": {
            "text": {
                "type": "string",
                "description": "Input string. Server caps at 4 MiB of UTF-8 bytes."
            }
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled schema must be an object")
}

// =====================================================================
// base64_encode
// =====================================================================

pub const TOOL_BASE64_ENCODE: &str = "encode_base64";

#[derive(Debug, Default, Clone)]
pub struct Base64EncodeTool;
impl Base64EncodeTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for Base64EncodeTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_BASE64_ENCODE.to_string(),
            description: "Base64-encode a UTF-8 string. `url_safe=true` \
                          selects the URL-safe alphabet (`-` and `_` \
                          instead of `+` and `/`)."
                .to_string(),
            input_schema: {
                let mut m = input_schema_with_text();
                m.get_mut("properties")
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert(
                        "url_safe".into(),
                        json!({"type": "boolean", "default": false}),
                    );
                m
            },
            output_schema: Some(
                json!({
                    "type": "object",
                    "required": ["encoded"],
                    "properties": {"encoded": {"type": "string"}},
                    "additionalProperties": false,
                })
                .as_object()
                .cloned()
                .expect("hand-rolled schema must be an object"),
            ),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let text = parse_input(&args, "text")?;
        let url_safe = parse_optional_bool(&args, "url_safe", false)?;
        Ok(json!({"encoded": encoders::base64_encode(&text, url_safe)}))
    }
}

// =====================================================================
// base64_decode
// =====================================================================

pub const TOOL_BASE64_DECODE: &str = "decode_base64";

#[derive(Debug, Default, Clone)]
pub struct Base64DecodeTool;
impl Base64DecodeTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for Base64DecodeTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_BASE64_DECODE.to_string(),
            description: "Decode a base64 string. Set `url_safe=true` if \
                          the input uses the URL-safe alphabet. Decoded \
                          bytes must be valid UTF-8."
                .to_string(),
            input_schema: {
                let mut m = input_schema_with_text();
                m.get_mut("properties")
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert(
                        "url_safe".into(),
                        json!({"type": "boolean", "default": false}),
                    );
                m
            },
            output_schema: Some(
                json!({
                    "type": "object",
                    "required": ["decoded"],
                    "properties": {"decoded": {"type": "string"}},
                    "additionalProperties": false,
                })
                .as_object()
                .cloned()
                .expect("hand-rolled schema must be an object"),
            ),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let text = parse_input(&args, "text")?;
        let url_safe = parse_optional_bool(&args, "url_safe", false)?;
        let decoded =
            encoders::base64_decode(&text, url_safe).map_err(ToolError::InvalidArguments)?;
        Ok(json!({"decoded": decoded}))
    }
}

// =====================================================================
// url_encode / url_decode — URL-component percent-encoding. Pass-
// through set is the RFC 3986 *unreserved* characters
// (alphanumerics + `-_.~`); stricter than JavaScript's
// encodeURIComponent (which also leaves `! * ' ( )` unescaped).
// =====================================================================

pub const TOOL_URL_ENCODE: &str = "encode_url_component";

#[derive(Debug, Default, Clone)]
pub struct UrlEncodeTool;
impl UrlEncodeTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for UrlEncodeTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_URL_ENCODE.to_string(),
            description: "Percent-encode a string for use as a URL \
                          query-component value. Pass-through set is \
                          the RFC 3986 *unreserved* characters \
                          (alphanumerics + `-_.~`); everything else \
                          (including `!*'()` which JavaScript's \
                          encodeURIComponent leaves alone) is \
                          %XX-escaped. The stricter behaviour is safer \
                          when the result is concatenated into a URL \
                          with no further escaping."
                .to_string(),
            input_schema: input_schema_with_text(),
            output_schema: Some(
                json!({
                    "type": "object",
                    "required": ["encoded"],
                    "properties": {"encoded": {"type": "string"}},
                    "additionalProperties": false,
                })
                .as_object()
                .cloned()
                .expect("hand-rolled schema must be an object"),
            ),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let text = parse_input(&args, "text")?;
        Ok(json!({"encoded": encoders::url_component_encode(&text)}))
    }
}

pub const TOOL_URL_DECODE: &str = "decode_url_component";

#[derive(Debug, Default, Clone)]
pub struct UrlDecodeTool;
impl UrlDecodeTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for UrlDecodeTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_URL_DECODE.to_string(),
            description: "Decode a percent-encoded URL component back to \
                          its original UTF-8 form. Accepts `+` as a \
                          space (form-encoded variant) for compatibility \
                          with browser / Node behaviour."
                .to_string(),
            input_schema: input_schema_with_text(),
            output_schema: Some(
                json!({
                    "type": "object",
                    "required": ["decoded"],
                    "properties": {"decoded": {"type": "string"}},
                    "additionalProperties": false,
                })
                .as_object()
                .cloned()
                .expect("hand-rolled schema must be an object"),
            ),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let text = parse_input(&args, "text")?;
        let decoded = encoders::url_component_decode(&text).map_err(ToolError::InvalidArguments)?;
        Ok(json!({"decoded": decoded}))
    }
}

// =====================================================================
// hex_encode / hex_decode
// =====================================================================

pub const TOOL_HEX_ENCODE: &str = "encode_hex";

#[derive(Debug, Default, Clone)]
pub struct HexEncodeTool;
impl HexEncodeTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for HexEncodeTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_HEX_ENCODE.to_string(),
            description: "Encode a UTF-8 string as lower-case hexadecimal \
                          bytes (e.g. `\"abc\"` → `\"616263\"`)."
                .to_string(),
            input_schema: input_schema_with_text(),
            output_schema: Some(
                json!({
                    "type": "object",
                    "required": ["encoded"],
                    "properties": {"encoded": {"type": "string"}},
                    "additionalProperties": false,
                })
                .as_object()
                .cloned()
                .expect("hand-rolled schema must be an object"),
            ),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let text = parse_input(&args, "text")?;
        Ok(json!({"encoded": encoders::hex_encode(&text)}))
    }
}

pub const TOOL_HEX_DECODE: &str = "decode_hex";

#[derive(Debug, Default, Clone)]
pub struct HexDecodeTool;
impl HexDecodeTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for HexDecodeTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_HEX_DECODE.to_string(),
            description: "Decode a hexadecimal string (either case) back \
                          to its UTF-8 source. Rejects odd-length input \
                          and non-hex characters."
                .to_string(),
            input_schema: input_schema_with_text(),
            output_schema: Some(
                json!({
                    "type": "object",
                    "required": ["decoded"],
                    "properties": {"decoded": {"type": "string"}},
                    "additionalProperties": false,
                })
                .as_object()
                .cloned()
                .expect("hand-rolled schema must be an object"),
            ),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let text = parse_input(&args, "text")?;
        let decoded = encoders::hex_decode(&text).map_err(ToolError::InvalidArguments)?;
        Ok(json!({"decoded": decoded}))
    }
}

// =====================================================================
// jwt_decode (no signature verification)
// =====================================================================

pub const TOOL_JWT_DECODE: &str = "decode_jwt_unverified";

#[derive(Debug, Default, Clone)]
pub struct JwtDecodeTool;
impl JwtDecodeTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for JwtDecodeTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_JWT_DECODE.to_string(),
            description: "Decode a JWT into its three segments (header, \
                          payload, signature_present). This tool DOES \
                          NOT verify the signature; never trust the \
                          decoded claims for authorisation decisions. \
                          For verification, pair this with a dedicated \
                          auth library that knows the signing key."
                .to_string(),
            input_schema: {
                let mut m = Map::new();
                m.insert("type".into(), json!("object"));
                m.insert("required".into(), json!(["token"]));
                m.insert(
                    "properties".into(),
                    json!({
                        "token": {
                            "type": "string",
                            "minLength": 1,
                            "description": "JWT in dot-separated form: header.payload.signature. Server caps at 4 MiB of UTF-8 bytes."
                        }
                    }),
                );
                m.insert("additionalProperties".into(), json!(false));
                m
            },
            output_schema: Some(
                json!({
                    "type": "object",
                    "required": ["header", "payload", "signature_present"],
                    "properties": {
                        "header": {"type": "object"},
                        "payload": {"type": "object"},
                        "signature_present": {"type": "boolean"}
                    },
                    "additionalProperties": false,
                })
                .as_object()
                .cloned()
                .expect("hand-rolled schema must be an object"),
            ),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let token = parse_input(&args, "token")?;
        let parts = encoders::jwt_decode_unverified(&token).map_err(ToolError::InvalidArguments)?;
        Ok(json!({
            "header": parts.header,
            "payload": parts.payload,
            "signature_present": parts.signature_present,
        }))
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
    fn base64_round_trip_via_tools() {
        let enc = run(&Base64EncodeTool::new(), json!({"text": "Hello"})).unwrap();
        let dec = run(
            &Base64DecodeTool::new(),
            json!({"text": enc["encoded"].as_str().unwrap()}),
        )
        .unwrap();
        assert_eq!(dec["decoded"], "Hello");
    }

    #[test]
    fn url_round_trip_via_tools() {
        let enc = run(&UrlEncodeTool::new(), json!({"text": "a=b&c=d"})).unwrap();
        assert_eq!(enc["encoded"], "a%3Db%26c%3Dd");
        let dec = run(
            &UrlDecodeTool::new(),
            json!({"text": enc["encoded"].as_str().unwrap()}),
        )
        .unwrap();
        assert_eq!(dec["decoded"], "a=b&c=d");
    }

    #[test]
    fn hex_round_trip_via_tools() {
        let enc = run(&HexEncodeTool::new(), json!({"text": "abc"})).unwrap();
        assert_eq!(enc["encoded"], "616263");
        let dec = run(&HexDecodeTool::new(), json!({"text": "616263"})).unwrap();
        assert_eq!(dec["decoded"], "abc");
    }

    #[test]
    fn jwt_decode_happy_path() {
        let token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let out = run(&JwtDecodeTool::new(), json!({"token": token})).unwrap();
        assert_eq!(out["payload"]["sub"], "1234567890");
        assert_eq!(out["signature_present"], true);
    }

    #[test]
    fn jwt_decode_rejects_garbage() {
        let err = run(&JwtDecodeTool::new(), json!({"token": "not.a.jwt"})).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn input_size_cap_enforced() {
        let big = "a".repeat(MAX_INPUT_BYTES + 1);
        let err = run(&Base64EncodeTool::new(), json!({"text": big})).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }
}
