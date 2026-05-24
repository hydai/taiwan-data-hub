//! MCP wrappers for cryptographic hashers (sha256, blake3). Output
//! is lower-case hex; for non-text inputs use one of the encoders
//! first (e.g. base64-decode → hash).
//!
//! Both algorithms run over the raw UTF-8 bytes of the input
//! string. SHA-256 is the long-standing default; BLAKE3 is faster
//! and supports unbounded-output via XOF, but we expose the
//! standard 32-byte output here for parity with sha256.

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::json_helpers::kind_of;

const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;

fn parse_text(args: &Value) -> Result<String, ToolError> {
    match args.get("text") {
        Some(Value::String(s)) => {
            if s.len() > MAX_INPUT_BYTES {
                Err(ToolError::InvalidArguments(format!(
                    "`text` is {} bytes; maximum is {MAX_INPUT_BYTES}",
                    s.len()
                )))
            } else {
                Ok(s.clone())
            }
        }
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`text` must be a string, got {}",
            kind_of(other)
        ))),
        None => Err(ToolError::InvalidArguments("missing `text`".into())),
    }
}

fn input_schema() -> Map<String, Value> {
    // `maxLength` omitted on purpose — JSON Schema `maxLength`
    // counts code points but the runtime cap is in UTF-8 bytes.
    // See encoders_tool.rs for the same rationale.
    json!({
        "type": "object",
        "required": ["text"],
        "properties": {
            "text": {
                "type": "string",
                "description": "Input string. Hash runs over the raw UTF-8 bytes. Server caps at 16 MiB."
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
        "required": ["hex", "length_bytes"],
        "properties": {
            "hex": {"type": "string"},
            "length_bytes": {"type": "integer"}
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled schema must be an object")
}

// =====================================================================
// hash_sha256
// =====================================================================

pub const TOOL_SHA256: &str = "hash_sha256";

#[derive(Debug, Default, Clone)]
pub struct Sha256Tool;
impl Sha256Tool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for Sha256Tool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_SHA256.to_string(),
            description: "SHA-256 of the input's UTF-8 bytes. Output is \
                          a 64-character lower-case hex string."
                .to_string(),
            input_schema: input_schema(),
            output_schema: Some(output_schema()),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let text = parse_text(&args)?;
        let mut hasher = Sha256::new();
        hasher.update(text.as_bytes());
        let digest = hasher.finalize();
        Ok(json!({
            "hex": hex::encode(digest),
            "length_bytes": digest.len(),
        }))
    }
}

// =====================================================================
// hash_blake3
// =====================================================================

pub const TOOL_BLAKE3: &str = "hash_blake3";

#[derive(Debug, Default, Clone)]
pub struct Blake3Tool;
impl Blake3Tool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for Blake3Tool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_BLAKE3.to_string(),
            description: "BLAKE3 (default 32-byte output) of the input's \
                          UTF-8 bytes. Output is a 64-character lower-case \
                          hex string. BLAKE3 is faster than SHA-256 and \
                          has the same output size, so it's a drop-in \
                          replacement when interop with older systems \
                          isn't required."
                .to_string(),
            input_schema: input_schema(),
            output_schema: Some(output_schema()),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let text = parse_text(&args)?;
        let hash = blake3::hash(text.as_bytes());
        Ok(json!({
            "hex": hash.to_hex().to_string(),
            "length_bytes": 32,
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

    /// Canonical SHA-256 vector from NIST: empty string →
    /// e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
    #[test]
    fn sha256_empty_string() {
        let out = run(&Sha256Tool::new(), json!({"text": ""})).unwrap();
        assert_eq!(
            out["hex"],
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(out["length_bytes"], 32);
    }

    #[test]
    fn sha256_abc() {
        let out = run(&Sha256Tool::new(), json!({"text": "abc"})).unwrap();
        assert_eq!(
            out["hex"],
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// BLAKE3 of the empty string per the reference vectors:
    /// af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262
    #[test]
    fn blake3_empty_string() {
        let out = run(&Blake3Tool::new(), json!({"text": ""})).unwrap();
        assert_eq!(
            out["hex"],
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
        assert_eq!(out["length_bytes"], 32);
    }

    #[test]
    fn input_size_cap_enforced() {
        let big = "a".repeat(MAX_INPUT_BYTES + 1);
        let err = run(&Sha256Tool::new(), json!({"text": big})).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }
}
