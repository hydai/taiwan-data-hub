//! MCP wrapper for the `transcode_big5_utf8` tool (#6.10
//! follow-up). Thin facade over [`crate::big5`].
//!
//! One tool with a `direction` parameter rather than two
//! separate tools so a caller that doesn't know which way it's
//! transcoding can still pick the right shape at request time.
//! base64 is the natural way to carry Big5 bytes through a
//! JSON-RPC envelope (the bytes aren't valid UTF-8 so they
//! can't ride in a JSON string directly).

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};

use crate::big5;
use crate::json_helpers::kind_of;

pub const TOOL_NAME: &str = "transcode_big5_utf8";

/// Cap on `text` size — the same 4 MiB ceiling the encoders and
/// JSON tools share. Big5 has at most 2 bytes per Han character,
/// so 4 MiB carries a couple of million characters, well above
/// any reasonable single-document workload.
const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;

const DIR_BIG5_TO_UTF8: &str = "big5_to_utf8";
const DIR_UTF8_TO_BIG5: &str = "utf8_to_big5";

#[derive(Debug, Default, Clone)]
pub struct TranscodeBig5Utf8Tool;
impl TranscodeBig5Utf8Tool {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for TranscodeBig5Utf8Tool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: "Transcode between Big5 (HKSCS-augmented per WHATWG) \
                and UTF-8. `direction = \"big5_to_utf8\"` decodes a \
                base64-encoded Big5 byte stream into a UTF-8 string; \
                `direction = \"utf8_to_big5\"` encodes a UTF-8 string into \
                base64-encoded Big5 bytes. Invalid Big5 sequences decode as \
                U+FFFD; UTF-8 code points outside the Big5 repertoire encode \
                as numeric character references (`&#NNN;`). Both cases set \
                `had_replacements: true` so callers can flag lossy round-trips."
                .to_string(),
            input_schema: input_schema(),
            output_schema: Some(output_schema()),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let text = parse_text(&args)?;
        let direction = parse_direction(&args)?;
        match direction {
            DIR_BIG5_TO_UTF8 => {
                // `text` carries base64-encoded Big5 bytes — the
                // only safe way to round-trip non-UTF-8 bytes
                // through a JSON string envelope.
                let bytes = BASE64.decode(&text).map_err(|e| {
                    ToolError::InvalidArguments(format!(
                        "`text` must be valid base64 for direction=big5_to_utf8: {e}"
                    ))
                })?;
                let result = big5::decode_big5_to_utf8(&bytes);
                Ok(json!({
                    "output": result.output,
                    "had_replacements": result.had_replacements,
                }))
            }
            DIR_UTF8_TO_BIG5 => {
                let result = big5::encode_utf8_to_big5(&text);
                let encoded = BASE64.encode(&result.output);
                Ok(json!({
                    "output": encoded,
                    "had_replacements": result.had_replacements,
                }))
            }
            other => Err(ToolError::InvalidArguments(format!(
                "`direction` must be `{DIR_BIG5_TO_UTF8}` or `{DIR_UTF8_TO_BIG5}`; got {other:?}"
            ))),
        }
    }
}

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

fn parse_direction(args: &Value) -> Result<&'static str, ToolError> {
    match args.get("direction") {
        Some(Value::String(s)) => match s.as_str() {
            DIR_BIG5_TO_UTF8 => Ok(DIR_BIG5_TO_UTF8),
            DIR_UTF8_TO_BIG5 => Ok(DIR_UTF8_TO_BIG5),
            other => Err(ToolError::InvalidArguments(format!(
                "`direction` must be `{DIR_BIG5_TO_UTF8}` or `{DIR_UTF8_TO_BIG5}`; got {other:?}"
            ))),
        },
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`direction` must be a string, got {}",
            kind_of(other)
        ))),
        None => Err(ToolError::InvalidArguments("missing `direction`".into())),
    }
}

fn input_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["text", "direction"],
        "properties": {
            "text": {
                "type": "string",
                "description": "Source text. For direction=big5_to_utf8 this \
                    must be base64-encoded Big5 bytes (raw bytes can't ride \
                    in a JSON string). For direction=utf8_to_big5 this is \
                    the UTF-8 string to encode. Server caps at 4 MiB."
            },
            "direction": {
                "type": "string",
                "enum": [DIR_BIG5_TO_UTF8, DIR_UTF8_TO_BIG5],
                "description": "Transcoding direction."
            }
        },
        "additionalProperties": false
    })
    .as_object()
    .cloned()
    .expect("hand-rolled schema must be an object")
}

fn output_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["output", "had_replacements"],
        "properties": {
            "output": {
                "type": "string",
                "description": "For direction=big5_to_utf8 this is the \
                    decoded UTF-8 string. For direction=utf8_to_big5 this \
                    is base64-encoded Big5 bytes."
            },
            "had_replacements": {
                "type": "boolean",
                "description": "True when the transcoder substituted U+FFFD \
                    (Big5 → UTF-8, invalid sequence) or a numeric character \
                    reference (UTF-8 → Big5, unmappable code point). The \
                    round-trip is information-lossy in that case."
            }
        },
        "additionalProperties": false
    })
    .as_object()
    .cloned()
    .expect("hand-rolled schema must be an object")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(future)
    }

    // Same canonical sequence the pure-module tests pin —
    // exercising it through the wrapper too proves the base64
    // layer is wired correctly.
    const HELLO_BIG5: &[u8] = &[0xA7, 0x41, 0xA6, 0x6E];
    const HELLO_UTF8: &str = "你好";

    #[test]
    fn descriptor_advertises_required_fields() {
        let d = TranscodeBig5Utf8Tool::new().descriptor();
        assert_eq!(d.name, TOOL_NAME);
        let required = d.input_schema["required"].as_array().unwrap();
        let names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(names, vec!["text", "direction"]);
    }

    #[test]
    fn big5_to_utf8_round_trips_canonical_sequence() {
        let b64 = BASE64.encode(HELLO_BIG5);
        let out = block_on(TranscodeBig5Utf8Tool::new().call(json!({
            "text": b64,
            "direction": DIR_BIG5_TO_UTF8
        })))
        .expect("call");
        assert_eq!(out["output"], HELLO_UTF8);
        assert_eq!(out["had_replacements"], false);
    }

    #[test]
    fn utf8_to_big5_round_trips_canonical_sequence() {
        let out = block_on(TranscodeBig5Utf8Tool::new().call(json!({
            "text": HELLO_UTF8,
            "direction": DIR_UTF8_TO_BIG5
        })))
        .expect("call");
        let bytes = BASE64
            .decode(out["output"].as_str().unwrap())
            .expect("base64 round-trip");
        assert_eq!(bytes, HELLO_BIG5);
        assert_eq!(out["had_replacements"], false);
    }

    #[test]
    fn malformed_base64_in_big5_direction_is_invalid_arguments() {
        let err = block_on(TranscodeBig5Utf8Tool::new().call(json!({
            "text": "this-is-not-base64!@#$",
            "direction": DIR_BIG5_TO_UTF8
        })))
        .expect_err("must fail");
        match err {
            ToolError::InvalidArguments(msg) => assert!(
                msg.contains("base64"),
                "expected base64 diagnostic, got {msg:?}",
            ),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[test]
    fn unmappable_codepoint_flags_replacements_via_ncr() {
        // Emoji aren't in Big5 — the encoder substitutes a
        // numeric character reference. End-to-end through the
        // wrapper, that means the output is base64 of ASCII
        // bytes containing `&#128512;` and the flag is true.
        let out = block_on(TranscodeBig5Utf8Tool::new().call(json!({
            "text": "hello 😀",
            "direction": DIR_UTF8_TO_BIG5
        })))
        .expect("call");
        let bytes = BASE64.decode(out["output"].as_str().unwrap()).unwrap();
        let as_text = std::str::from_utf8(&bytes).expect("ASCII via NCR");
        assert!(
            as_text.contains("&#128512;"),
            "expected NCR substitution, got {as_text:?}",
        );
        assert_eq!(out["had_replacements"], true);
    }

    #[test]
    fn unknown_direction_value_is_invalid_arguments() {
        let err = block_on(TranscodeBig5Utf8Tool::new().call(json!({
            "text": "x",
            "direction": "gb2312_to_utf8"
        })))
        .expect_err("must fail");
        match err {
            ToolError::InvalidArguments(msg) => {
                assert!(msg.contains("direction"), "got {msg:?}");
            }
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[test]
    fn missing_or_wrong_type_args_are_invalid_arguments() {
        let err = block_on(TranscodeBig5Utf8Tool::new().call(json!({}))).expect_err("missing all");
        assert!(matches!(err, ToolError::InvalidArguments(_)));

        let err = block_on(TranscodeBig5Utf8Tool::new().call(json!({
            "text": "x"
        })))
        .expect_err("missing direction");
        assert!(matches!(err, ToolError::InvalidArguments(_)));

        let err = block_on(TranscodeBig5Utf8Tool::new().call(json!({
            "direction": DIR_UTF8_TO_BIG5
        })))
        .expect_err("missing text");
        assert!(matches!(err, ToolError::InvalidArguments(_)));

        let err = block_on(TranscodeBig5Utf8Tool::new().call(json!({
            "text": 42,
            "direction": DIR_UTF8_TO_BIG5
        })))
        .expect_err("wrong-type text");
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn oversize_text_is_rejected_before_transcoding() {
        let huge = "x".repeat(MAX_INPUT_BYTES + 1);
        let err = block_on(TranscodeBig5Utf8Tool::new().call(json!({
            "text": huge,
            "direction": DIR_UTF8_TO_BIG5
        })))
        .expect_err("must fail");
        match err {
            ToolError::InvalidArguments(msg) => assert!(
                msg.contains("text") && msg.contains("maximum"),
                "expected size-cap message, got {msg:?}",
            ),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }
}
