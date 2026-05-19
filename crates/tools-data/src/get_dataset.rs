//! `get_dataset` MCP tool — returns the full read view for a single
//! dataset, looked up by UUID or slug, with i18n fields resolved
//! against the caller's locale.

use std::sync::Arc;

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};
use storage::{DatasetFileRow, DatasetFull, DatasetKey, DatasetReader, DatasetVersionRow};
use uuid::Uuid;

const DEFAULT_LOCALE: &str = "zh-TW";

/// MCP tool name. Stable identifier — clients pin to this string.
pub const TOOL_NAME: &str = "get_dataset";

/// Reads from any [`DatasetReader`]; `mcp-stdio` and `gateway` plug in
/// a `storage::Storage` at startup, tests plug in an in-memory stub.
#[derive(Clone)]
pub struct GetDatasetTool {
    reader: Arc<dyn DatasetReader>,
}

impl GetDatasetTool {
    pub fn new<R: DatasetReader>(reader: R) -> Self {
        Self {
            reader: Arc::new(reader),
        }
    }

    pub fn from_arc(reader: Arc<dyn DatasetReader>) -> Self {
        Self { reader }
    }
}

impl std::fmt::Debug for GetDatasetTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GetDatasetTool").finish_non_exhaustive()
    }
}

#[async_trait]
impl ToolHandler for GetDatasetTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: "Fetch one dataset's full metadata, version history, and file list. \
                          Specify either `id` (UUID) or `slug` (kebab-case marketplace path); \
                          exactly one is required. i18n fields render in the requested locale, \
                          with zh-TW as the fallback."
                .to_string(),
            input_schema: input_schema(),
            output_schema: Some(output_schema()),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let req = Request::parse(&args)?;
        let key = req.key();
        let full = self
            .reader
            .get_dataset(key)
            .await
            .map_err(|e| ToolError::Execution(format!("storage: {e}")))?;
        let Some(full) = full else {
            return Err(ToolError::NotFound(format!(
                "dataset not found ({})",
                req.lookup_str()
            )));
        };
        Ok(render_full(&full, &req.locale))
    }
}

/// Parsed-and-validated `tools/call` arguments.
struct Request {
    id: Option<Uuid>,
    slug: Option<String>,
    locale: String,
}

impl Request {
    fn parse(args: &Value) -> Result<Self, ToolError> {
        let obj = args
            .as_object()
            .ok_or_else(|| ToolError::InvalidArguments("arguments must be a JSON object".into()))?;

        let id = match obj.get("id") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s.is_empty() => None,
            Some(Value::String(s)) => Some(Uuid::parse_str(s).map_err(|e| {
                ToolError::InvalidArguments(format!("`id` is not a valid UUID: {e}"))
            })?),
            Some(other) => {
                return Err(ToolError::InvalidArguments(format!(
                    "`id` must be a string, got {}",
                    kind_of(other)
                )));
            }
        };

        let slug = match obj.get("slug") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s.is_empty() => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(other) => {
                return Err(ToolError::InvalidArguments(format!(
                    "`slug` must be a string, got {}",
                    kind_of(other)
                )));
            }
        };

        match (id, &slug) {
            (None, None) => {
                return Err(ToolError::InvalidArguments(
                    "exactly one of `id` or `slug` is required".into(),
                ));
            }
            (Some(_), Some(_)) => {
                return Err(ToolError::InvalidArguments(
                    "specify only one of `id` or `slug`, not both".into(),
                ));
            }
            _ => {}
        }

        let locale = match obj.get("locale") {
            None | Some(Value::Null) => DEFAULT_LOCALE.to_string(),
            Some(Value::String(s)) if s.is_empty() => DEFAULT_LOCALE.to_string(),
            Some(Value::String(s)) => s.clone(),
            Some(other) => {
                return Err(ToolError::InvalidArguments(format!(
                    "`locale` must be a string, got {}",
                    kind_of(other)
                )));
            }
        };

        Ok(Self { id, slug, locale })
    }

    fn key(&self) -> DatasetKey {
        match (&self.id, &self.slug) {
            (Some(id), _) => DatasetKey::id(*id),
            (None, Some(slug)) => DatasetKey::slug(slug.clone()),
            // parse() ensures one or the other is present.
            (None, None) => unreachable!("validated in Request::parse"),
        }
    }

    fn lookup_str(&self) -> String {
        match (&self.id, &self.slug) {
            (Some(id), _) => format!("id={id}"),
            (None, Some(s)) => format!("slug={s}"),
            (None, None) => unreachable!(),
        }
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

/// Render `DatasetFull` into the schema-shaped JSON the tool advertises.
fn render_full(full: &DatasetFull, locale: &str) -> Value {
    let d = &full.dataset;
    let mut dataset = Map::new();
    dataset.insert("id".into(), Value::String(d.id.to_string()));
    dataset.insert("source".into(), Value::String(d.source.clone()));
    dataset.insert("source_id".into(), Value::String(d.source_id.clone()));
    dataset.insert("slug".into(), Value::String(d.slug.clone()));
    dataset.insert("tier".into(), Value::String(d.tier.clone()));
    dataset.insert("license".into(), Value::String(d.license.clone()));
    dataset.insert(
        "name".into(),
        resolve_i18n(&d.title_i18n, locale).map_or(Value::Null, Value::String),
    );
    if let Some(desc) = d
        .description_i18n
        .as_ref()
        .and_then(|v| resolve_i18n(v, locale))
    {
        dataset.insert("description".into(), Value::String(desc));
    }
    if let Some(p) = &d.publisher {
        dataset.insert("publisher".into(), Value::String(p.clone()));
    }
    if let Some(f) = &d.update_frequency {
        dataset.insert("update_frequency".into(), Value::String(f.clone()));
    }
    if let Some(url) = &d.original_url {
        dataset.insert("original_url".into(), Value::String(url.clone()));
    }
    if let Some(schema) = &d.schema_json {
        dataset.insert("schema".into(), schema.clone());
    }
    if let Some(n) = d.row_count_estimate {
        dataset.insert("row_count_estimate".into(), Value::Number(n.into()));
    }
    dataset.insert(
        "last_modified_at".into(),
        Value::String(d.last_modified_at.to_rfc3339()),
    );
    dataset.insert(
        "first_seen_at".into(),
        Value::String(d.first_seen_at.to_rfc3339()),
    );

    let versions: Vec<Value> = full
        .versions
        .iter()
        .map(|vwf| render_version(&vwf.version, &vwf.files))
        .collect();

    json!({
        "locale": locale,
        "dataset": Value::Object(dataset),
        "versions": versions,
    })
}

fn render_version(v: &DatasetVersionRow, files: &[DatasetFileRow]) -> Value {
    let mut out = Map::new();
    out.insert("id".into(), Value::String(v.id.to_string()));
    out.insert("version".into(), Value::String(v.version.clone()));
    out.insert(
        "fetched_at".into(),
        Value::String(v.fetched_at.to_rfc3339()),
    );
    if let Some(c) = &v.checksum {
        out.insert("checksum".into(), Value::String(c.clone()));
    }
    if let Some(n) = v.row_count {
        out.insert("row_count".into(), Value::Number(n.into()));
    }
    if let Some(diff) = &v.schema_diff {
        out.insert("schema_diff".into(), diff.clone());
    }
    out.insert(
        "files".into(),
        Value::Array(files.iter().map(render_file).collect()),
    );
    Value::Object(out)
}

fn render_file(f: &DatasetFileRow) -> Value {
    let mut out = Map::new();
    out.insert("format".into(), Value::String(f.format.clone()));
    out.insert("uri".into(), Value::String(f.uri.clone()));
    if let Some(n) = f.byte_size {
        out.insert("byte_size".into(), Value::Number(n.into()));
    }
    if let Some(c) = &f.checksum {
        out.insert("checksum".into(), Value::String(c.clone()));
    }
    Value::Object(out)
}

/// Pick the best string for `locale` from a jsonb i18n map, falling
/// back to zh-TW per CLAUDE.md. Returns `None` when both the
/// requested locale and zh-TW are missing or non-string.
fn resolve_i18n(value: &Value, locale: &str) -> Option<String> {
    let obj = value.as_object()?;
    let pick = |key: &str| {
        obj.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    pick(locale).or_else(|| pick("zh-TW")).map(str::to_owned)
}

fn input_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "properties": {
            "id": {
                "type": "string",
                "description": "Dataset UUID. Mutually exclusive with `slug`.",
            },
            "slug": {
                "type": "string",
                "description": "Marketplace slug (`datasets.slug`). Mutually exclusive with `id`.",
            },
            "locale": {
                "type": "string",
                "description": "BCP-47 locale. Falls back to zh-TW when the requested locale \
                                 is missing for a given field. Defaults to zh-TW.",
                "examples": ["zh-TW", "en"],
            }
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("input schema must be an object")
}

fn output_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["locale", "dataset", "versions"],
        "properties": {
            "locale": { "type": "string" },
            "dataset": {
                "type": "object",
                "required": ["id", "source", "source_id", "slug", "tier", "license", "name", "last_modified_at", "first_seen_at"],
                "properties": {
                    "id":                 { "type": "string" },
                    "source":             { "type": "string" },
                    "source_id":          { "type": "string" },
                    "slug":               { "type": "string" },
                    "tier":               { "type": "string", "enum": ["platinum", "gold", "silver", "bronze"] },
                    "license":            { "type": "string" },
                    "name":               { "type": ["string", "null"] },
                    "description":        { "type": "string" },
                    "publisher":          { "type": "string" },
                    "update_frequency":   { "type": "string" },
                    "original_url":       { "type": "string" },
                    "schema":             { "type": "object" },
                    "row_count_estimate": { "type": "integer", "minimum": 0 },
                    "last_modified_at":   { "type": "string", "format": "date-time" },
                    "first_seen_at":      { "type": "string", "format": "date-time" }
                },
                "additionalProperties": false
            },
            "versions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["id", "version", "fetched_at", "files"],
                    "properties": {
                        "id":          { "type": "string" },
                        "version":     { "type": "string" },
                        "fetched_at":  { "type": "string", "format": "date-time" },
                        "checksum":    { "type": "string" },
                        "row_count":   { "type": "integer", "minimum": 0 },
                        "schema_diff": { "type": "object" },
                        "files": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "required": ["format", "uri"],
                                "properties": {
                                    "format":    { "type": "string" },
                                    "uri":       { "type": "string" },
                                    "byte_size": { "type": "integer", "minimum": 0 },
                                    "checksum":  { "type": "string" }
                                },
                                "additionalProperties": false
                            }
                        }
                    },
                    "additionalProperties": false
                }
            }
        },
        "additionalProperties": false
    })
    .as_object()
    .cloned()
    .expect("output schema must be an object")
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::{DateTime, Utc};
    use std::collections::BTreeMap;
    use storage::{
        DatasetFileRow, DatasetFull, DatasetKey, DatasetReader, DatasetRow, DatasetVersionRow,
        StorageError, VersionWithFiles,
    };

    /// In-memory reader for unit tests — no Postgres needed.
    struct StubReader {
        by_id: BTreeMap<Uuid, DatasetFull>,
        by_slug: BTreeMap<String, Uuid>,
    }

    impl StubReader {
        fn new(full: DatasetFull) -> Self {
            let mut by_id = BTreeMap::new();
            let mut by_slug = BTreeMap::new();
            let id = full.dataset.id;
            let slug = full.dataset.slug.clone();
            by_id.insert(id, full);
            by_slug.insert(slug, id);
            Self { by_id, by_slug }
        }
        fn empty() -> Self {
            Self {
                by_id: BTreeMap::new(),
                by_slug: BTreeMap::new(),
            }
        }
    }

    #[async_trait]
    impl DatasetReader for StubReader {
        async fn get_dataset(&self, key: DatasetKey) -> Result<Option<DatasetFull>, StorageError> {
            let id = match key {
                DatasetKey::Id(id) => Some(id),
                DatasetKey::Slug(slug) => self.by_slug.get(&slug).copied(),
            };
            Ok(id.and_then(|i| self.by_id.get(&i).cloned()))
        }
    }

    fn fixed_time() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-04-15T03:30:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn sample_full() -> DatasetFull {
        let dataset_id = Uuid::parse_str("019071a2-e1cb-7c11-9c1d-3e8a01b87000").unwrap();
        let version_id = Uuid::parse_str("019071a2-e1cb-7c11-9c1d-3e8a01b87100").unwrap();
        let file_id = Uuid::parse_str("019071a2-e1cb-7c11-9c1d-3e8a01b87200").unwrap();
        DatasetFull {
            dataset: DatasetRow {
                id: dataset_id,
                source: "data_gov_tw".into(),
                source_id: "11102".into(),
                slug: "real-estate-prices".into(),
                domain_id: 1,
                title_i18n: json!({"zh-TW": "實價登錄價格", "en": "Real estate prices"}),
                description_i18n: Some(json!({"zh-TW": "全國不動產交易實價揭露"})),
                tier: "bronze".into(),
                license: "政府資料開放授權條款-第1版".into(),
                publisher: Some("內政部地政司".into()),
                update_frequency: Some("每月更新".into()),
                original_url: Some("https://data.gov.tw/dataset/real-estate-prices".into()),
                schema_json: None,
                row_count_estimate: Some(10_000),
                last_modified_at: fixed_time(),
                first_seen_at: fixed_time(),
            },
            versions: vec![VersionWithFiles {
                version: DatasetVersionRow {
                    id: version_id,
                    dataset_id,
                    version: "2026.04".into(),
                    fetched_at: fixed_time(),
                    checksum: Some("abc123".into()),
                    row_count: Some(10_000),
                    schema_diff: None,
                },
                files: vec![DatasetFileRow {
                    id: file_id,
                    dataset_version_id: version_id,
                    format: "csv".into(),
                    uri: "s3://bucket/file.csv".into(),
                    byte_size: Some(2048),
                    checksum: Some("def456".into()),
                }],
            }],
        }
    }

    fn invoke(reader: impl DatasetReader, args: Value) -> Result<Value, ToolError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(GetDatasetTool::new(reader).call(args))
    }

    #[test]
    fn descriptor_carries_input_and_output_schemas() {
        let d = GetDatasetTool::new(StubReader::empty()).descriptor();
        assert_eq!(d.name, "get_dataset");
        assert!(d.output_schema.is_some());
    }

    #[test]
    fn lookup_by_id_returns_full_view_in_default_locale() {
        let full = sample_full();
        let id = full.dataset.id;
        let out = invoke(StubReader::new(full), json!({"id": id.to_string()})).expect("ok");
        assert_eq!(out["locale"], "zh-TW");
        assert_eq!(out["dataset"]["slug"], "real-estate-prices");
        assert_eq!(out["dataset"]["name"], "實價登錄價格");
        assert_eq!(out["dataset"]["description"], "全國不動產交易實價揭露");
        assert_eq!(out["dataset"]["tier"], "bronze");
        assert_eq!(out["versions"].as_array().unwrap().len(), 1);
        assert_eq!(out["versions"][0]["version"], "2026.04");
        assert_eq!(out["versions"][0]["files"][0]["format"], "csv");
        assert_eq!(out["versions"][0]["files"][0]["byte_size"], 2048);
    }

    #[test]
    fn lookup_by_slug_resolves_locale() {
        let out = invoke(
            StubReader::new(sample_full()),
            json!({"slug": "real-estate-prices", "locale": "en"}),
        )
        .expect("ok");
        assert_eq!(out["locale"], "en");
        assert_eq!(out["dataset"]["name"], "Real estate prices");
        // description has no `en`, falls back to zh-TW.
        assert_eq!(out["dataset"]["description"], "全國不動產交易實價揭露");
    }

    #[test]
    fn unknown_id_returns_not_found_error() {
        let err = invoke(
            StubReader::empty(),
            json!({"id": "00000000-0000-0000-0000-000000000001"}),
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::NotFound(_)));
    }

    #[test]
    fn unknown_slug_returns_not_found_error() {
        let err = invoke(StubReader::empty(), json!({"slug": "nope"})).unwrap_err();
        assert!(matches!(err, ToolError::NotFound(_)));
    }

    #[test]
    fn missing_both_id_and_slug_is_invalid_arguments() {
        let err = invoke(StubReader::empty(), json!({})).unwrap_err();
        match err {
            ToolError::InvalidArguments(msg) => assert!(msg.contains("required")),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[test]
    fn both_id_and_slug_is_invalid_arguments() {
        let err = invoke(
            StubReader::empty(),
            json!({"id": "00000000-0000-0000-0000-000000000001", "slug": "x"}),
        )
        .unwrap_err();
        match err {
            ToolError::InvalidArguments(msg) => assert!(msg.contains("not both")),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[test]
    fn malformed_uuid_is_invalid_arguments() {
        let err = invoke(StubReader::empty(), json!({"id": "not-a-uuid"})).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn non_string_locale_is_invalid_arguments() {
        let err = invoke(StubReader::empty(), json!({"slug": "any", "locale": 42})).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn locale_falls_back_to_zh_tw_for_unknown_language() {
        let out = invoke(
            StubReader::new(sample_full()),
            json!({"slug": "real-estate-prices", "locale": "ja"}),
        )
        .expect("ok");
        assert_eq!(out["locale"], "ja");
        // Both fields fall back to zh-TW
        assert_eq!(out["dataset"]["name"], "實價登錄價格");
        assert_eq!(out["dataset"]["description"], "全國不動產交易實價揭露");
    }
}
