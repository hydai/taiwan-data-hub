//! `search_datasets` MCP tool — multi-criteria search over the catalog.
//!
//! Per DESIGN.md §9 (#1.5): filters by `q` (中文 / 英文 full-text +
//! trigram), `domain` (slug), `tier`, `license`, `locale`. Returns
//! paginated hits with i18n fields already resolved against the
//! requested locale (zh-TW fallback). The storage layer clamps
//! `limit` to ≤ 100 — see `storage::SearchParams::MAX_LIMIT`.

use std::sync::Arc;

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};
use storage::{DatasetSearcher, SearchHit, SearchPage, SearchParams};

const DEFAULT_LOCALE: &str = "zh-TW";

/// MCP tool name. Stable identifier — clients pin to this string.
pub const TOOL_NAME: &str = "search_datasets";

/// Reads from any [`DatasetSearcher`]; production code plugs in a
/// `storage::Storage`, tests plug in an in-memory stub.
#[derive(Clone)]
pub struct SearchDatasetsTool {
    searcher: Arc<dyn DatasetSearcher>,
}

impl SearchDatasetsTool {
    pub fn new<S: DatasetSearcher>(searcher: S) -> Self {
        Self {
            searcher: Arc::new(searcher),
        }
    }

    pub fn from_arc(searcher: Arc<dyn DatasetSearcher>) -> Self {
        Self { searcher }
    }
}

impl std::fmt::Debug for SearchDatasetsTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchDatasetsTool").finish_non_exhaustive()
    }
}

#[async_trait]
impl ToolHandler for SearchDatasetsTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: "Search the dataset catalog. \
                          Combine free-text `q` (works for English and Chinese) with optional \
                          `domain` (slug), `tier`, and `license` filters. `limit` is capped at 100; \
                          pass back `next_offset` to page through results. i18n fields are resolved \
                          against the requested `locale`, falling back to zh-TW."
                .to_string(),
            input_schema: input_schema(),
            output_schema: Some(output_schema()),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let req = Request::parse(&args)?;
        let page = self
            .searcher
            .search_datasets(req.into_params())
            .await
            .map_err(|e| ToolError::Execution(format!("storage: {e}")))?;
        Ok(render_page(&page))
    }
}

/// Parsed-and-validated `tools/call` arguments. Mirrors the shape of
/// [`storage::SearchParams`] but with strict per-field type checks so
/// a malformed client gets a clear `InvalidArguments` rather than
/// silently dropping the filter.
struct Request {
    q: Option<String>,
    domain: Option<String>,
    tier: Option<String>,
    license: Option<String>,
    locale: String,
    limit: u32,
    offset: u32,
}

impl Request {
    fn parse(args: &Value) -> Result<Self, ToolError> {
        let obj = args
            .as_object()
            .ok_or_else(|| ToolError::InvalidArguments("arguments must be a JSON object".into()))?;

        let q = optional_string(obj, "q")?;
        let domain = optional_string(obj, "domain")?;
        let tier = optional_string(obj, "tier")?;
        if let Some(t) = tier.as_deref()
            && !matches!(t, "platinum" | "gold" | "silver" | "bronze")
        {
            return Err(ToolError::InvalidArguments(format!(
                "`tier` must be one of platinum/gold/silver/bronze, got {t:?}",
            )));
        }
        let license = optional_string(obj, "license")?;
        let locale = optional_string(obj, "locale")?.unwrap_or_else(|| DEFAULT_LOCALE.to_owned());

        let limit = optional_u32(obj, "limit")?.unwrap_or(SearchParams::DEFAULT_LIMIT);
        if limit > SearchParams::MAX_LIMIT {
            // Reject the over-limit case here so a tool client sees a
            // clear error rather than being silently clamped by the
            // storage layer (which is a defence in depth, not the
            // primary message).
            return Err(ToolError::InvalidArguments(format!(
                "`limit` must be ≤ {} (got {limit})",
                SearchParams::MAX_LIMIT,
            )));
        }
        let offset = optional_u32(obj, "offset")?.unwrap_or(0);

        Ok(Self {
            q,
            domain,
            tier,
            license,
            locale,
            limit,
            offset,
        })
    }

    fn into_params(self) -> SearchParams {
        SearchParams {
            q: self.q,
            domain: self.domain,
            tier: self.tier,
            license: self.license,
            locale: Some(self.locale),
            limit: self.limit,
            offset: self.offset,
        }
    }
}

/// Read `obj[key]` as `Option<String>`, treating absent / null / empty
/// as `None` and surfacing wrong-type inputs as `InvalidArguments`.
fn optional_string(obj: &Map<String, Value>, key: &str) -> Result<Option<String>, ToolError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`{key}` must be a string, got {}",
            kind_of(other),
        ))),
    }
}

/// Read `obj[key]` as `Option<u32>`. Accepts JSON numbers in the
/// unsigned 32-bit range; rejects negatives, fractions, and over-range
/// values with a precise message.
fn optional_u32(obj: &Map<String, Value>, key: &str) -> Result<Option<u32>, ToolError> {
    let Some(value) = obj.get(key) else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::Number(n) => {
            let as_u64 = n.as_u64().ok_or_else(|| {
                ToolError::InvalidArguments(format!(
                    "`{key}` must be a non-negative integer, got {n}",
                ))
            })?;
            u32::try_from(as_u64).map(Some).map_err(|_| {
                ToolError::InvalidArguments(format!("`{key}` must fit in u32, got {as_u64}"))
            })
        }
        other => Err(ToolError::InvalidArguments(format!(
            "`{key}` must be a number, got {}",
            kind_of(other),
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

/// Shape one row for the JSON response. Pulled out of `render_page`
/// so unit tests can exercise it directly.
fn render_hit(h: &SearchHit) -> Value {
    let mut obj = Map::with_capacity(8);
    obj.insert("id".into(), Value::String(h.id.to_string()));
    obj.insert("slug".into(), Value::String(h.slug.clone()));
    obj.insert("title".into(), Value::String(h.title.clone()));
    obj.insert(
        "description".into(),
        h.description.clone().map_or(Value::Null, Value::String),
    );
    obj.insert("domain".into(), Value::String(h.domain_slug.clone()));
    obj.insert("tier".into(), Value::String(h.tier.clone()));
    obj.insert("license".into(), Value::String(h.license.clone()));
    obj.insert(
        "publisher".into(),
        h.publisher.clone().map_or(Value::Null, Value::String),
    );
    Value::Object(obj)
}

fn render_page(page: &SearchPage) -> Value {
    let hits: Vec<Value> = page.hits.iter().map(render_hit).collect();
    json!({
        "hits": hits,
        "next_offset": page.next_offset.map_or(Value::Null, |o| json!(o)),
    })
}

/// MCP input schema. Kept verbose because LLM clients use the
/// description text to choose tools; clearer wording → better tool
/// selection.
fn input_schema() -> Map<String, Value> {
    let mut props = Map::new();
    props.insert(
        "q".into(),
        json!({
            "type": "string",
            "description": "Free-text query. Searches title, description, and publisher. Works for both English keywords and Chinese substrings.",
        }),
    );
    props.insert(
        "domain".into(),
        json!({
            "type": "string",
            "description": "Domain slug to filter by (one of the 20 internal domains; see `list_domains`).",
        }),
    );
    props.insert(
        "tier".into(),
        json!({
            "type": "string",
            "enum": ["platinum", "gold", "silver", "bronze"],
            "description": "Quality tier filter.",
        }),
    );
    props.insert(
        "license".into(),
        json!({
            "type": "string",
            "description": "Exact license string filter (e.g., \"CC-BY-4.0\").",
        }),
    );
    props.insert(
        "locale".into(),
        json!({
            "type": "string",
            "description": "BCP-47 locale tag used to render `title` and `description` (default: zh-TW). Unknown locales fall back to zh-TW.",
            "default": DEFAULT_LOCALE,
        }),
    );
    props.insert(
        "limit".into(),
        json!({
            "type": "integer",
            "minimum": 1,
            "maximum": SearchParams::MAX_LIMIT,
            "default": SearchParams::DEFAULT_LIMIT,
            "description": "Max hits to return. Capped at 100.",
        }),
    );
    props.insert(
        "offset".into(),
        json!({
            "type": "integer",
            "minimum": 0,
            "default": 0,
            "description": "Rows to skip; use the `next_offset` returned by the previous call.",
        }),
    );
    let mut schema = Map::new();
    schema.insert("type".into(), Value::String("object".into()));
    schema.insert("additionalProperties".into(), Value::Bool(false));
    schema.insert("properties".into(), Value::Object(props));
    schema
}

/// MCP output schema. Declares both `hits[]` shape and the nullable
/// `next_offset` cursor so a strict client can validate the response.
fn output_schema() -> Map<String, Value> {
    let hit_props = json!({
        "id": {"type": "string", "format": "uuid"},
        "slug": {"type": "string"},
        "title": {"type": "string"},
        "description": {"type": ["string", "null"]},
        "domain": {"type": "string"},
        "tier": {"type": "string"},
        "license": {"type": "string"},
        "publisher": {"type": ["string", "null"]},
    });
    let mut schema = Map::new();
    schema.insert("type".into(), Value::String("object".into()));
    schema.insert(
        "required".into(),
        Value::Array(vec![Value::String("hits".into())]),
    );
    schema.insert(
        "properties".into(),
        json!({
            "hits": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["id", "slug", "title", "domain", "tier", "license"],
                    "properties": hit_props,
                },
            },
            "next_offset": {
                "type": ["integer", "null"],
                "minimum": 0,
                "description": "Pass this back as `offset` to fetch the next page; null when no more rows.",
            },
        }),
    );
    schema
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use mcp_core::ToolError;
    use storage::StorageError;
    use uuid::Uuid;

    /// Arc-backed in-memory `DatasetSearcher`. Both the test scope and
    /// the `SearchDatasetsTool` (which consumes the searcher into its
    /// own `Arc`) share the same `last_params` slot, so post-call
    /// assertions work without indirection.
    #[derive(Clone)]
    struct StubSearcher {
        last_params: Arc<Mutex<Option<SearchParams>>>,
        response: Arc<Mutex<SearchPage>>,
    }

    impl StubSearcher {
        fn new(response: SearchPage) -> Self {
            Self {
                last_params: Arc::new(Mutex::new(None)),
                response: Arc::new(Mutex::new(response)),
            }
        }

        fn last_params(&self) -> SearchParams {
            self.last_params
                .lock()
                .unwrap()
                .clone()
                .expect("`call` was invoked before this assertion")
        }
    }

    #[async_trait]
    impl DatasetSearcher for StubSearcher {
        async fn search_datasets(&self, params: SearchParams) -> Result<SearchPage, StorageError> {
            *self.last_params.lock().unwrap() = Some(params);
            Ok(self.response.lock().unwrap().clone())
        }
    }

    fn empty_searcher() -> StubSearcher {
        StubSearcher::new(SearchPage {
            hits: vec![],
            next_offset: None,
        })
    }

    fn fixture_hit(slug: &str) -> SearchHit {
        SearchHit {
            id: Uuid::nil(),
            slug: slug.to_owned(),
            title: format!("{slug} title"),
            description: Some(format!("{slug} desc")),
            domain_slug: "environment".to_owned(),
            tier: "bronze".to_owned(),
            license: "CC-BY-4.0".to_owned(),
            publisher: Some("Agency".to_owned()),
        }
    }

    #[tokio::test]
    async fn happy_path_forwards_filters_and_renders_hits() {
        let stub = StubSearcher::new(SearchPage {
            hits: vec![fixture_hit("air-quality"), fixture_hit("forest-land")],
            next_offset: Some(2),
        });
        let tool = SearchDatasetsTool::new(stub.clone());

        let out = tool
            .call(json!({
                "q": "土地",
                "domain": "environment",
                "tier": "bronze",
                "license": "CC-BY-4.0",
                "locale": "en",
                "limit": 2,
                "offset": 0
            }))
            .await
            .expect("ok");

        assert_eq!(out["hits"].as_array().unwrap().len(), 2);
        assert_eq!(out["hits"][0]["slug"], "air-quality");
        assert_eq!(out["next_offset"], 2);

        // Filters were forwarded verbatim — sanitisation happens at
        // the storage layer, not in the tool.
        let params = stub.last_params();
        assert_eq!(params.q.as_deref(), Some("土地"));
        assert_eq!(params.domain.as_deref(), Some("environment"));
        assert_eq!(params.tier.as_deref(), Some("bronze"));
        assert_eq!(params.license.as_deref(), Some("CC-BY-4.0"));
        assert_eq!(params.locale.as_deref(), Some("en"));
        assert_eq!(params.limit, 2);
    }

    #[tokio::test]
    async fn locale_defaults_to_zh_tw_when_unset() {
        let stub = empty_searcher();
        let tool = SearchDatasetsTool::new(stub.clone());
        tool.call(json!({})).await.expect("ok");
        assert_eq!(stub.last_params().locale.as_deref(), Some("zh-TW"));
    }

    #[tokio::test]
    async fn null_filters_are_treated_as_unset() {
        let stub = empty_searcher();
        let tool = SearchDatasetsTool::new(stub.clone());
        tool.call(json!({"q": null, "tier": null, "license": null}))
            .await
            .expect("ok");
        let params = stub.last_params();
        assert!(params.q.is_none());
        assert!(params.tier.is_none());
        assert!(params.license.is_none());
    }

    #[tokio::test]
    async fn empty_strings_are_treated_as_unset() {
        let stub = empty_searcher();
        let tool = SearchDatasetsTool::new(stub.clone());
        tool.call(json!({"q": "", "domain": ""})).await.expect("ok");
        let params = stub.last_params();
        assert!(params.q.is_none());
        assert!(params.domain.is_none());
    }

    #[tokio::test]
    async fn limit_over_max_is_rejected() {
        let tool = SearchDatasetsTool::new(empty_searcher());
        let err = tool.call(json!({"limit": 101})).await.unwrap_err();
        match err {
            ToolError::InvalidArguments(m) => assert!(m.contains("≤ 100"), "got: {m}"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn negative_limit_is_rejected() {
        let tool = SearchDatasetsTool::new(empty_searcher());
        let err = tool.call(json!({"limit": -3})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn invalid_tier_is_rejected() {
        let tool = SearchDatasetsTool::new(empty_searcher());
        let err = tool.call(json!({"tier": "diamond"})).await.unwrap_err();
        match err {
            ToolError::InvalidArguments(m) => {
                assert!(m.contains("platinum/gold/silver/bronze"), "got: {m}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_object_args_rejected() {
        let tool = SearchDatasetsTool::new(empty_searcher());
        let err = tool.call(json!([])).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn output_schema_is_well_formed_object() {
        let schema = output_schema();
        assert_eq!(schema.get("type"), Some(&Value::String("object".into())));
        let props = schema.get("properties").and_then(Value::as_object).unwrap();
        assert!(props.contains_key("hits"));
        assert!(props.contains_key("next_offset"));
    }
}
