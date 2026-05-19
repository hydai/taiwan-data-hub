//! `list_domains` MCP tool — returns the 20 seeded domains, localized
//! against the caller's preferred locale.
//!
//! `dataset_count` is always `0` until the data.gov.tw crawler (#1.4)
//! and `search_datasets` (#1.5) land — at that point a future patch
//! will swap in a DB-backed counter.

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};

use crate::domains;

/// Default locale used when the caller omits `locale` or passes an empty
/// string. Matches CLAUDE.md's i18n contract: zh-TW is the source language.
const DEFAULT_LOCALE: &str = "zh-TW";

/// MCP tool name. Stable identifier — clients pin to this string.
pub const TOOL_NAME: &str = "list_domains";

/// State-free implementation of `list_domains`.
#[derive(Debug, Default, Clone)]
pub struct ListDomainsTool;

impl ListDomainsTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for ListDomainsTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: "List the 20 dataset domains shown in the Taiwan Data Hub \
                          marketplace. Each entry carries an i18n-resolved name, an \
                          optional short description, and the number of datasets \
                          currently catalogued under it."
                .to_string(),
            input_schema: input_schema(),
            output_schema: Some(output_schema()),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let locale = parse_locale(&args)?;

        let entries = domains::embedded()
            .iter()
            .map(|d| {
                let mut entry = Map::new();
                entry.insert("slug".into(), Value::String(d.slug.clone()));
                entry.insert("kind".into(), Value::String(d.kind.as_str().to_string()));
                entry.insert("sort_order".into(), Value::Number(d.sort_order.into()));
                entry.insert(
                    "name".into(),
                    Value::String(d.name.resolve(&locale).to_string()),
                );
                if let Some(desc) = &d.description {
                    entry.insert(
                        "description".into(),
                        Value::String(desc.resolve(&locale).to_string()),
                    );
                }
                // TODO(#1.5): replace 0 with a DB-backed count once
                // search_datasets is wired up to the populated catalog.
                entry.insert("dataset_count".into(), Value::Number(0u32.into()));
                Value::Object(entry)
            })
            .collect::<Vec<_>>();

        Ok(json!({
            "locale": locale,
            "domains": entries,
        }))
    }
}

/// Pull the `locale` argument from the JSON-RPC params, validate it's a
/// string, default empty → `zh-TW`. Anything else trips a clear
/// `InvalidArguments` rather than silently being ignored.
fn parse_locale(args: &Value) -> Result<String, ToolError> {
    let Some(raw) = args.get("locale") else {
        return Ok(DEFAULT_LOCALE.to_string());
    };
    match raw {
        Value::Null => Ok(DEFAULT_LOCALE.to_string()),
        Value::String(s) if s.is_empty() => Ok(DEFAULT_LOCALE.to_string()),
        Value::String(s) => Ok(s.clone()),
        other => Err(ToolError::InvalidArguments(format!(
            "`locale` must be a string, got {}",
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

fn input_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "properties": {
            "locale": {
                "type": "string",
                "description": "BCP-47 locale. Falls back to zh-TW when the requested \
                                 locale is missing for a given field. Defaults to zh-TW.",
                "examples": ["zh-TW", "en"],
            }
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled input schema must be an object")
}

fn output_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["locale", "domains"],
        "properties": {
            "locale": { "type": "string" },
            "domains": {
                "type": "array",
                "minItems": 20,
                "maxItems": 20,
                "items": {
                    "type": "object",
                    "required": ["slug", "kind", "sort_order", "name", "dataset_count"],
                    "properties": {
                        "slug": { "type": "string" },
                        "kind": { "type": "string", "enum": ["topical", "meta", "horizontal"] },
                        "sort_order": { "type": "integer", "minimum": 0 },
                        "name": { "type": "string", "minLength": 1 },
                        "description": { "type": "string" },
                        "dataset_count": { "type": "integer", "minimum": 0 }
                    },
                    "additionalProperties": false
                }
            }
        },
        "additionalProperties": false
    })
    .as_object()
    .cloned()
    .expect("hand-rolled output schema must be an object")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoke(args: Value) -> Value {
        // The tool is sync internally; spin up a single-threaded runtime
        // for the async surface.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(ListDomainsTool::new().call(args))
            .expect("call ok")
    }

    #[test]
    fn descriptor_advertises_input_and_output_schemas() {
        let d = ListDomainsTool::new().descriptor();
        assert_eq!(d.name, "list_domains");
        assert!(
            d.output_schema.is_some(),
            "output_schema is part of the DoD"
        );
        assert_eq!(d.input_schema["type"], json!("object"));
    }

    #[test]
    fn default_locale_is_zh_tw_when_omitted() {
        let out = invoke(json!({}));
        assert_eq!(out["locale"], "zh-TW");
        let first = &out["domains"][0];
        // Slug ordering follows sort_order; realestate-land has the smallest.
        assert_eq!(first["slug"], "realestate-land");
        assert_eq!(first["name"], "不動產與土地");
    }

    #[test]
    fn english_locale_returns_english_names() {
        let out = invoke(json!({"locale": "en"}));
        assert_eq!(out["locale"], "en");
        let first = &out["domains"][0];
        assert_eq!(first["name"], "Real estate & land");
    }

    #[test]
    fn unknown_locale_falls_back_to_zh_tw() {
        let out = invoke(json!({"locale": "ja"}));
        assert_eq!(out["locale"], "ja");
        let first = &out["domains"][0];
        // The name field falls back per-string; locale echoed verbatim.
        assert_eq!(first["name"], "不動產與土地");
    }

    #[test]
    fn returns_exactly_twenty_domains() {
        let out = invoke(json!({}));
        let arr = out["domains"].as_array().unwrap();
        assert_eq!(arr.len(), 20);
    }

    #[test]
    fn every_entry_has_dataset_count_zero_for_now() {
        let out = invoke(json!({}));
        for entry in out["domains"].as_array().unwrap() {
            assert_eq!(entry["dataset_count"], 0);
        }
    }

    #[test]
    fn empty_string_locale_treated_as_default() {
        let out = invoke(json!({"locale": ""}));
        assert_eq!(out["locale"], "zh-TW");
    }

    #[test]
    fn non_string_locale_is_invalid_arguments() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(ListDomainsTool::new().call(json!({"locale": 42})))
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    /// Cheap structural check: every output domain entry satisfies the
    /// required-keys / value-type contract declared in `output_schema`.
    /// A formal JSON-Schema validator (the `jsonschema` crate) would be
    /// more thorough but adds a non-trivial dep for a single tool — the
    /// schema is authored by hand here and round-tripped by serde, so the
    /// structural checks below cover what would otherwise be a 30-line
    /// validator setup.
    #[test]
    fn output_conforms_to_declared_schema_shape() {
        let out = invoke(json!({}));
        assert!(out["locale"].is_string());
        let arr = out["domains"].as_array().unwrap();
        for entry in arr {
            for key in ["slug", "kind", "sort_order", "name", "dataset_count"] {
                assert!(entry.get(key).is_some(), "missing {key}: {entry}");
            }
            assert!(entry["slug"].is_string());
            assert!(entry["name"].is_string());
            assert!(entry["sort_order"].is_i64());
            assert!(entry["dataset_count"].is_u64());
            let kind = entry["kind"].as_str().unwrap();
            assert!(matches!(kind, "topical" | "meta" | "horizontal"));
            // description is optional; when present it must be a string.
            if let Some(desc) = entry.get("description") {
                assert!(desc.is_string());
            }
        }
    }
}
