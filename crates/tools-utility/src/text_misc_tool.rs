//! MCP wrappers for the three miscellaneous text tools shipped in
//! #6.10 batch B: slugify, `regex_test`, `html_sanitize`.

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use regex::Regex;
use serde_json::{Value, json};

use crate::json_helpers::kind_of;

const MAX_TEXT_BYTES: usize = 1024 * 1024;
const MAX_HTML_BYTES: usize = 4 * 1024 * 1024;
/// Cap on `text_regex_test` match count so a pathological pattern
/// like `.` over a 1 MiB input doesn't return ~1 M JSON objects.
/// 1 k matches is plenty for the "tell me what this regex hits"
/// use case; beyond that callers want streaming, not MCP.
const MAX_REGEX_MATCHES: usize = 1024;

fn parse_text(args: &Value, key: &str, max: usize) -> Result<String, ToolError> {
    match args.get(key) {
        Some(Value::String(s)) => {
            if s.len() > max {
                Err(ToolError::InvalidArguments(format!(
                    "`{key}` is {} bytes; maximum is {max}",
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

// =====================================================================
// slugify
// =====================================================================

pub const TOOL_SLUGIFY: &str = "text_slugify";

#[derive(Debug, Default, Clone)]
pub struct SlugifyTool;
impl SlugifyTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for SlugifyTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_SLUGIFY.to_string(),
            description: "Convert a UTF-8 string to an ASCII-only, \
                          lower-case, hyphen-separated slug suitable for \
                          URLs, filenames, and DB keys. Latin-extended \
                          chars are best-effort transliterated (café → \
                          cafe, naïve → naive); characters without an \
                          ASCII mapping (most CJK, emoji, etc.) are \
                          dropped."
                .to_string(),
            // No `maxLength` in the schema (it counts code points;
            // the runtime cap is bytes — they disagree for multi-
            // byte UTF-8). Server enforces 1 MiB byte cap in
            // `parse_text`.
            input_schema: json!({
                "type": "object",
                "required": ["text"],
                "properties": {
                    "text": {"type": "string", "description": "Input string; server caps at 1 MiB of UTF-8 bytes."}
                },
                "additionalProperties": false,
            })
            .as_object()
            .cloned()
            .expect("hand-rolled schema must be an object"),
            output_schema: Some(
                json!({
                    "type": "object",
                    "required": ["slug"],
                    "properties": {"slug": {"type": "string"}},
                    "additionalProperties": false,
                })
                .as_object()
                .cloned()
                .expect("hand-rolled schema must be an object"),
            ),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let text = parse_text(&args, "text", MAX_TEXT_BYTES)?;
        Ok(json!({"slug": slug::slugify(&text)}))
    }
}

// =====================================================================
// regex_test
// =====================================================================

pub const TOOL_REGEX_TEST: &str = "text_regex_test";

#[derive(Debug, Default, Clone)]
pub struct RegexTestTool;
impl RegexTestTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for RegexTestTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_REGEX_TEST.to_string(),
            description: "Test whether a Rust-flavour regex matches the \
                          input text, and return up to 1024 matches \
                          (start byte offset, length, matched \
                          substring). Uses the `regex` crate's syntax \
                          (no look-around, guaranteed linear-time \
                          matching). When the pattern matches more \
                          than 1024 times the response carries \
                          `truncated: true` and the first 1024 matches \
                          only; `match_count` reflects what was \
                          returned, not the true count. A malformed \
                          pattern is reported as `InvalidArguments`."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "required": ["pattern", "text"],
                "properties": {
                    "pattern": {"type": "string", "minLength": 1,
                                "description": "Rust-regex pattern; server caps at 1024 UTF-8 bytes."},
                    "text": {"type": "string", "description": "Input text; server caps at 1 MiB of UTF-8 bytes."}
                },
                "additionalProperties": false,
            })
            .as_object()
            .cloned()
            .expect("hand-rolled schema must be an object"),
            output_schema: Some(
                json!({
                    "type": "object",
                    "required": ["match_count", "matches", "truncated"],
                    "properties": {
                        "match_count": {"type": "integer"},
                        "truncated": {"type": "boolean"},
                        "matches": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "required": ["start", "length", "text"],
                                "properties": {
                                    "start": {"type": "integer"},
                                    "length": {"type": "integer"},
                                    "text": {"type": "string"}
                                }
                            }
                        }
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
        let pattern = parse_text(&args, "pattern", 1024)?;
        let text = parse_text(&args, "text", MAX_TEXT_BYTES)?;
        let re = Regex::new(&pattern)
            .map_err(|e| ToolError::InvalidArguments(format!("invalid regex: {e}")))?;
        // Cap the iteration explicitly so a pathological pattern
        // (e.g. `.` over a 1 MiB input → ~1 M matches) can't blow
        // up the response size. We `take(MAX_REGEX_MATCHES + 1)`
        // and then check the length: if we got MAX+1 there were
        // strictly more matches than we kept.
        let collected: Vec<_> = re.find_iter(&text).take(MAX_REGEX_MATCHES + 1).collect();
        let truncated = collected.len() > MAX_REGEX_MATCHES;
        let matches: Vec<Value> = collected
            .into_iter()
            .take(MAX_REGEX_MATCHES)
            .map(|m| {
                json!({
                    "start": m.start(),
                    "length": m.end() - m.start(),
                    "text": m.as_str(),
                })
            })
            .collect();
        Ok(json!({
            "match_count": matches.len(),
            "truncated": truncated,
            "matches": matches,
        }))
    }
}

// =====================================================================
// html_sanitize
// =====================================================================

pub const TOOL_HTML_SANITIZE: &str = "html_sanitize";

#[derive(Debug, Default, Clone)]
pub struct HtmlSanitizeTool;
impl HtmlSanitizeTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for HtmlSanitizeTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_HTML_SANITIZE.to_string(),
            description: "Sanitize an HTML fragment using `ammonia`'s \
                          default allowlist — removes script tags, \
                          event handlers, inline styles, javascript: \
                          / data: URLs, and any tag/attribute not on \
                          the safe list. Returns the rewritten HTML. \
                          Suitable for displaying user-submitted \
                          rich text without an XSS risk."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "required": ["html"],
                "properties": {
                    "html": {"type": "string", "description": "HTML fragment; server caps at 4 MiB of UTF-8 bytes."}
                },
                "additionalProperties": false,
            })
            .as_object()
            .cloned()
            .expect("hand-rolled schema must be an object"),
            output_schema: Some(
                json!({
                    "type": "object",
                    "required": ["sanitized"],
                    "properties": {"sanitized": {"type": "string"}},
                    "additionalProperties": false,
                })
                .as_object()
                .cloned()
                .expect("hand-rolled schema must be an object"),
            ),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let html = parse_text(&args, "html", MAX_HTML_BYTES)?;
        Ok(json!({"sanitized": ammonia::clean(&html)}))
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
    fn slugify_basic_ascii() {
        let out = run(&SlugifyTool::new(), json!({"text": "Hello, World!"})).unwrap();
        assert_eq!(out["slug"], "hello-world");
    }

    #[test]
    fn slugify_strips_unicode() {
        // Latin-extended chars (é, è, à, ñ, etc.) are best-effort
        // transliterated to ASCII by the `slug` crate (café →
        // cafe, résumé → resume). Chars without a mapping (most
        // CJK, emoji, mathematical symbols) are dropped. The
        // assertion is intentionally weak — ASCII-lowercase +
        // hyphens only — so future `slug` releases can change
        // exact mappings without breaking the test.
        let out = run(&SlugifyTool::new(), json!({"text": "café — résumé"})).unwrap();
        let slug = out["slug"].as_str().unwrap();
        assert!(slug.chars().all(|c| c.is_ascii_lowercase() || c == '-'));
        assert!(!slug.is_empty());
    }

    #[test]
    fn regex_basic_match() {
        let out = run(
            &RegexTestTool::new(),
            json!({"pattern": r"\d+", "text": "abc 123 def 456"}),
        )
        .unwrap();
        assert_eq!(out["match_count"], 2);
        assert_eq!(out["matches"][0]["text"], "123");
        assert_eq!(out["matches"][1]["start"], 12);
    }

    #[test]
    fn regex_no_match_is_not_error() {
        let out = run(
            &RegexTestTool::new(),
            json!({"pattern": r"\d+", "text": "no digits here"}),
        )
        .unwrap();
        assert_eq!(out["match_count"], 0);
    }

    #[test]
    fn regex_invalid_pattern_is_error() {
        let err = run(
            &RegexTestTool::new(),
            json!({"pattern": "[unclosed", "text": "x"}),
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn html_sanitize_drops_script() {
        let out = run(
            &HtmlSanitizeTool::new(),
            json!({"html": "<p>safe</p><script>alert(1)</script>"}),
        )
        .unwrap();
        let sanitized = out["sanitized"].as_str().unwrap();
        assert!(sanitized.contains("<p>safe</p>"));
        assert!(!sanitized.contains("script"));
    }
}
