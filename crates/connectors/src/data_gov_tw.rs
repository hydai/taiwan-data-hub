//! CKAN-style catalog connector for `data.gov.tw`.
//!
//! Hits the upstream REST endpoint and translates each dataset into a
//! [`DatasetMetadata`]. Pagination is offset-based; the cursor encodes
//! `"<offset>:<limit>"`. Network errors and non-2xx responses surface as
//! [`ConnectorError`]. The decoder is *schema-drift tolerant*: unknown
//! keys are ignored and missing optional fields become `None`. JSON
//! that isn't syntactically valid still fails decode (as a
//! [`ConnectorError::Decode`]) — there's no recovery from a malformed
//! response body, only from a slowly-evolving schema.
//!
//! This module handles #1.4a (read-only HTTP). DB writes, retry/DLQ,
//! and `ETag` / `If-Modified-Since` are layered in later sub-issues.

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Deserialize;
use url::Url;

use crate::{Cursor, DatasetMetadata, Page, SourceConnector, SourceId};

const DEFAULT_BASE_URL: &str = "https://data.gov.tw";
const DEFAULT_PATH: &str = "/api/v2/rest/dataset";
const DEFAULT_LIMIT: u32 = 100;
/// data.gov.tw is occasionally slow under load. 60s is generous enough
/// to ride out short upstream pauses without letting one stuck request
/// block the whole crawl. Override via `Builder::timeout_secs` for
/// tests or for restricted-network environments.
const DEFAULT_TIMEOUT_SECS: u64 = 60;
const USER_AGENT: &str = concat!(
    "taiwan-data-hub/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/hydai/taiwan-data-hub)"
);

/// HTTP client for the data.gov.tw CKAN catalog.
#[derive(Debug, Clone)]
pub struct DataGovTwConnector {
    http: Client,
    base_url: Url,
    page_size: u32,
}

impl DataGovTwConnector {
    /// Build a connector pointed at the public data.gov.tw endpoint with
    /// sensible defaults. Use [`Self::builder`] to customise the base
    /// URL (for testing) or the page size.
    pub fn new() -> Result<Self, BuildError> {
        Self::builder().build()
    }

    pub fn builder() -> Builder {
        Builder::default()
    }

    fn build_request_url(&self, offset: u64, limit: u32) -> Result<Url, BuildError> {
        let mut url = self
            .base_url
            .join(DEFAULT_PATH)
            .map_err(|e| BuildError::InvalidUrl(e.to_string()))?;
        url.query_pairs_mut()
            .append_pair("limit", &limit.to_string())
            .append_pair("offset", &offset.to_string());
        Ok(url)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    #[error("HTTP client could not be constructed: {0}")]
    Client(#[from] reqwest::Error),
}

/// Builder for [`DataGovTwConnector`]. Only used directly in tests
/// (point at a wiremock server) and in `etl-worker` once #1.4c lands.
#[derive(Debug)]
pub struct Builder {
    base_url: String,
    page_size: u32,
    timeout_secs: u64,
}

impl Default for Builder {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            page_size: DEFAULT_LIMIT,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
        }
    }
}

impl Builder {
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub fn page_size(mut self, n: u32) -> Self {
        self.page_size = n.max(1);
        self
    }

    pub fn timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    pub fn build(self) -> Result<DataGovTwConnector, BuildError> {
        let base_url =
            Url::parse(&self.base_url).map_err(|e| BuildError::InvalidUrl(e.to_string()))?;
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .build()?;
        Ok(DataGovTwConnector {
            http,
            base_url,
            page_size: self.page_size,
        })
    }
}

#[async_trait]
impl SourceConnector for DataGovTwConnector {
    fn source_id(&self) -> SourceId {
        SourceId::DataGovTw
    }

    async fn list_datasets(&self, cursor: Option<Cursor>) -> Result<Page, crate::ConnectorError> {
        // `limit` is taken from the cursor when present so a resumed walk
        // keeps its original page size even if the connector is rebuilt
        // with a different `page_size`. On a fresh walk we fall back to
        // the connector's configured default.
        let (offset, limit) = parse_cursor(cursor.as_ref(), self.page_size)?;
        let url = self
            .build_request_url(offset, limit)
            .map_err(|e| crate::ConnectorError::Config(e.to_string()))?;

        tracing::debug!(%url, offset, limit, "GET data.gov.tw catalog");

        let resp = self.http.get(url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(crate::ConnectorError::BadStatus {
                status: status.as_u16(),
                body,
            });
        }

        let envelope: CkanEnvelope = resp
            .json()
            .await
            .map_err(|e| crate::ConnectorError::Decode(e.to_string()))?;

        if !envelope.success {
            return Err(crate::ConnectorError::Decode(
                "upstream reported success=false".to_owned(),
            ));
        }

        let result = envelope
            .result
            .ok_or_else(|| crate::ConnectorError::Decode("missing `result`".to_owned()))?;

        let items = result
            .results
            .into_iter()
            .map(into_metadata)
            .collect::<Vec<_>>();

        let total = result.count;
        let fetched = offset.saturating_add(items.len() as u64);
        // An empty page ALWAYS terminates the walk, regardless of what
        // `total` claims. Without this guard, upstream returning
        // `count: t` with `results: []` while `offset < t` (the
        // upstream inconsistency case) would re-emit the same cursor
        // and trap callers in an infinite loop. Log a warning so the
        // inconsistency is at least visible in production logs.
        if items.is_empty() {
            if let Some(t) = total {
                if offset < t {
                    tracing::warn!(
                        offset,
                        total = t,
                        "upstream returned empty page while count > offset; terminating walk to avoid infinite loop",
                    );
                }
            }
            return Ok(Page {
                items,
                next: None,
                total,
            });
        }
        let next = match total {
            Some(t) if fetched < t => Some(Cursor::new(format!("{fetched}:{limit}"))),
            None => Some(Cursor::new(format!("{fetched}:{limit}"))),
            _ => None,
        };

        Ok(Page { items, next, total })
    }
}

/// Decode a cursor into `(offset, limit)`. A `None` cursor yields the
/// fresh-walk defaults `(0, default_limit)`. A malformed cursor — wrong
/// shape, non-numeric components, or a zero `limit` — is reported as
/// [`crate::ConnectorError::InvalidCursor`].
fn parse_cursor(
    cursor: Option<&Cursor>,
    default_limit: u32,
) -> Result<(u64, u32), crate::ConnectorError> {
    let Some(c) = cursor else {
        return Ok((0, default_limit));
    };
    let (off_str, lim_str) =
        c.as_str()
            .split_once(':')
            .ok_or_else(|| crate::ConnectorError::InvalidCursor {
                connector: SourceId::DataGovTw,
                reason: format!("expected `<offset>:<limit>`, got `{}`", c.as_str()),
            })?;
    let offset: u64 = off_str
        .parse()
        .map_err(|e| crate::ConnectorError::InvalidCursor {
            connector: SourceId::DataGovTw,
            reason: format!("offset not a u64: {e}"),
        })?;
    let limit: u32 = lim_str
        .parse()
        .map_err(|e| crate::ConnectorError::InvalidCursor {
            connector: SourceId::DataGovTw,
            reason: format!("limit not a u32: {e}"),
        })?;
    if limit == 0 {
        return Err(crate::ConnectorError::InvalidCursor {
            connector: SourceId::DataGovTw,
            reason: "limit must be > 0".to_owned(),
        });
    }
    Ok((offset, limit))
}

fn into_metadata(raw: CkanDataset) -> DatasetMetadata {
    let mut title_i18n = BTreeMap::new();
    title_i18n.insert("zh-TW".to_owned(), raw.title.clone());
    if let Some(en) = non_empty(raw.title_en) {
        title_i18n.insert("en".to_owned(), en);
    }

    let mut description_i18n = BTreeMap::new();
    if let Some(notes) = non_empty(raw.notes) {
        description_i18n.insert("zh-TW".to_owned(), notes);
    }
    if let Some(en) = non_empty(raw.notes_en) {
        description_i18n.insert("en".to_owned(), en);
    }

    // For each group, pick the first non-empty human-readable label
    // (title → display_name → name). Without the `non_empty`
    // normalisation an empty `title: ""` would win against a
    // populated `display_name` and the entry would silently drop out
    // at the outer filter.
    let categories = raw
        .groups
        .into_iter()
        .filter_map(|g| {
            non_empty(g.title)
                .or_else(|| non_empty(g.display_name))
                .or_else(|| non_empty(Some(g.name)))
        })
        .collect();

    let original_url = Some(format!("https://data.gov.tw/dataset/{}", raw.name));

    DatasetMetadata {
        source_id: raw.id,
        slug: raw.name,
        title_i18n,
        description_i18n,
        license: non_empty(raw.license_title)
            .or_else(|| non_empty(raw.license_id))
            .unwrap_or_else(|| "unspecified".to_owned()),
        publisher: raw
            .organization
            .and_then(|o| non_empty(o.title).or_else(|| non_empty(Some(o.name)))),
        update_frequency: non_empty(raw.frequency),
        original_url,
        last_modified_at: raw
            .metadata_modified
            .as_deref()
            .and_then(parse_ckan_timestamp),
        upstream_categories: categories,
    }
}

/// Treat `Some("")` as `None`. CKAN sources frequently emit empty
/// strings for "unset" fields; if we left them through, `Option::or`
/// chains would short-circuit on the empty string and bypass the
/// intended fallback (e.g. `title → display_name → name`).
fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|x| !x.is_empty())
}

/// CKAN's `metadata_modified` is ISO-8601 without a timezone suffix —
/// always UTC in practice. Try the strict RFC-3339 form first, fall
/// back to the naive form when upstream drops the suffix.
fn parse_ckan_timestamp(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|naive| naive.and_utc())
        })
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(|naive| naive.and_utc())
        })
}

// ── CKAN response shapes ─────────────────────────────────────────────
//
// Lenient by design: `#[serde(default)]` on every optional field so a
// new key upstream doesn't break the connector, and a missing optional
// field is just `None`/empty rather than a decode error.

#[derive(Debug, Deserialize)]
struct CkanEnvelope {
    success: bool,
    #[serde(default)]
    result: Option<CkanResult>,
}

#[derive(Debug, Deserialize)]
struct CkanResult {
    #[serde(default)]
    count: Option<u64>,
    #[serde(default)]
    results: Vec<CkanDataset>,
}

#[derive(Debug, Deserialize)]
struct CkanDataset {
    id: String,
    name: String,
    title: String,
    #[serde(default)]
    title_en: Option<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    notes_en: Option<String>,
    #[serde(default)]
    license_title: Option<String>,
    #[serde(default)]
    license_id: Option<String>,
    #[serde(default)]
    organization: Option<CkanOrg>,
    #[serde(default)]
    groups: Vec<CkanGroup>,
    #[serde(default)]
    frequency: Option<String>,
    #[serde(default)]
    metadata_modified: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CkanOrg {
    name: String,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CkanGroup {
    name: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn sample_envelope(count: u64, datasets: &[serde_json::Value]) -> serde_json::Value {
        json!({
            "success": true,
            "result": { "count": count, "results": datasets }
        })
    }

    fn sample_dataset() -> serde_json::Value {
        json!({
            "id": "11102",
            "name": "real-estate-prices",
            "title": "實價登錄價格",
            "notes": "全國不動產交易實價揭露",
            "license_title": "政府資料開放授權條款-第1版",
            "organization": {"name": "moi", "title": "內政部地政司"},
            "groups": [
                {"name": "economy", "title": "經濟產業"},
                {"name": "land", "title": "土地"}
            ],
            "frequency": "每月更新",
            "metadata_modified": "2026-04-15T03:30:00"
        })
    }

    async fn mock_server_with_pages(
        page0: serde_json::Value,
        page1: Option<serde_json::Value>,
    ) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/rest/dataset"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page0))
            .mount(&server)
            .await;
        if let Some(p1) = page1 {
            Mock::given(method("GET"))
                .and(path("/api/v2/rest/dataset"))
                .and(query_param("offset", "2"))
                .respond_with(ResponseTemplate::new(200).set_body_json(p1))
                .mount(&server)
                .await;
        }
        server
    }

    fn connector(server: &MockServer) -> DataGovTwConnector {
        DataGovTwConnector::builder()
            .base_url(server.uri())
            .page_size(2)
            .build()
            .expect("build")
    }

    #[tokio::test]
    async fn parses_a_single_page_into_dataset_metadata() {
        let server = mock_server_with_pages(sample_envelope(1, &[sample_dataset()]), None).await;
        let c = connector(&server);
        let page = c.list_datasets(None).await.expect("ok");

        assert_eq!(page.total, Some(1));
        assert!(
            page.next.is_none(),
            "single-page result must have no next cursor"
        );
        assert_eq!(page.items.len(), 1);
        let d = &page.items[0];
        assert_eq!(d.source_id, "11102");
        assert_eq!(d.slug, "real-estate-prices");
        assert_eq!(
            d.title_i18n.get("zh-TW").map(String::as_str),
            Some("實價登錄價格")
        );
        assert_eq!(
            d.description_i18n.get("zh-TW").map(String::as_str),
            Some("全國不動產交易實價揭露")
        );
        assert_eq!(d.license, "政府資料開放授權條款-第1版");
        assert_eq!(d.publisher.as_deref(), Some("內政部地政司"));
        assert_eq!(d.update_frequency.as_deref(), Some("每月更新"));
        assert_eq!(
            d.original_url.as_deref(),
            Some("https://data.gov.tw/dataset/real-estate-prices")
        );
        assert!(d.last_modified_at.is_some());
        assert_eq!(d.upstream_categories, vec!["經濟產業", "土地"]);
    }

    #[tokio::test]
    async fn paginates_when_total_exceeds_page_size() {
        // page 0: two items of three; page 1: one item.
        let page0 = sample_envelope(3, &[sample_dataset(), sample_dataset()]);
        let page1 = sample_envelope(3, &[sample_dataset()]);
        let server = mock_server_with_pages(page0, Some(page1)).await;
        let c = connector(&server);

        let p0 = c.list_datasets(None).await.expect("p0");
        assert_eq!(p0.items.len(), 2);
        let next = p0.next.clone().expect("page-0 must hand back a cursor");
        assert_eq!(next.as_str(), "2:2");

        let p1 = c.list_datasets(Some(next)).await.expect("p1");
        assert_eq!(p1.items.len(), 1);
        assert!(p1.next.is_none(), "final page must terminate the walk");
    }

    /// Regression for Copilot PR #94 round 4: if upstream lies about
    /// the total (`count: 100`) and returns an empty page mid-walk,
    /// our previous logic re-emitted the same cursor — `fetched ==
    /// offset` so `fetched < total` stayed true forever. The fix
    /// treats an empty page as terminal regardless of what `total`
    /// claims.
    #[tokio::test]
    async fn empty_page_terminates_even_when_total_claims_more() {
        let server = MockServer::start().await;
        // Upstream claims count=100 but returns an empty page for offset=50.
        Mock::given(method("GET"))
            .and(path("/api/v2/rest/dataset"))
            .and(query_param("offset", "50"))
            .and(query_param("limit", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(sample_envelope(100, &[])))
            .mount(&server)
            .await;

        let page = connector(&server)
            .list_datasets(Some(Cursor::new("50:2")))
            .await
            .expect("ok");
        assert!(page.items.is_empty());
        assert!(
            page.next.is_none(),
            "empty page must terminate even when total > offset",
        );
    }

    #[tokio::test]
    async fn empty_results_terminate_when_total_is_unknown() {
        let envelope = json!({
            "success": true,
            "result": { "results": [] }
        });
        let server = mock_server_with_pages(envelope, None).await;
        let c = connector(&server);
        let page = c.list_datasets(None).await.expect("ok");
        assert!(page.items.is_empty());
        assert!(page.next.is_none(), "empty page must signal end-of-stream");
    }

    #[tokio::test]
    async fn non_200_response_surfaces_as_bad_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/rest/dataset"))
            .respond_with(ResponseTemplate::new(503).set_body_string("upstream is down"))
            .mount(&server)
            .await;
        let err = connector(&server).list_datasets(None).await.unwrap_err();
        match err {
            crate::ConnectorError::BadStatus { status, body } => {
                assert_eq!(status, 503);
                assert!(body.contains("down"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn upstream_success_false_is_a_decode_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/rest/dataset"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": false,
                "error": {"message": "rate limited"}
            })))
            .mount(&server)
            .await;
        let err = connector(&server).list_datasets(None).await.unwrap_err();
        assert!(matches!(err, crate::ConnectorError::Decode(_)));
    }

    #[tokio::test]
    async fn missing_optional_fields_decode_cleanly() {
        // Strip every optional field; only id/name/title remain.
        let envelope = sample_envelope(
            1,
            &[json!({
                "id": "9001",
                "name": "minimal",
                "title": "minimal-zh"
            })],
        );
        let server = mock_server_with_pages(envelope, None).await;
        let d = connector(&server)
            .list_datasets(None)
            .await
            .unwrap()
            .items
            .remove(0);
        assert_eq!(d.source_id, "9001");
        assert_eq!(d.license, "unspecified");
        assert!(d.publisher.is_none());
        assert!(d.upstream_categories.is_empty());
        assert!(d.last_modified_at.is_none());
        assert!(d.description_i18n.is_empty());
    }

    #[tokio::test]
    async fn malformed_cursor_yields_invalid_cursor_error() {
        let server = MockServer::start().await;
        let err = connector(&server)
            .list_datasets(Some(Cursor::new("not-a-cursor")))
            .await
            .unwrap_err();
        assert!(matches!(err, crate::ConnectorError::InvalidCursor { .. }));
    }

    #[tokio::test]
    async fn cursor_limit_overrides_connector_page_size_on_resume() {
        // Connector configured page_size = 2, but the cursor encodes
        // limit = 7 — the request must use 7 so a walk resumes with the
        // same chunk size it started with.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/rest/dataset"))
            .and(query_param("offset", "100"))
            .and(query_param("limit", "7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(sample_envelope(200, &[])))
            .mount(&server)
            .await;

        let _ = connector(&server)
            .list_datasets(Some(Cursor::new("100:7")))
            .await
            .expect("matching mock");
    }

    #[tokio::test]
    async fn zero_limit_in_cursor_is_invalid() {
        let server = MockServer::start().await;
        let err = connector(&server)
            .list_datasets(Some(Cursor::new("0:0")))
            .await
            .unwrap_err();
        match err {
            crate::ConnectorError::InvalidCursor { reason, .. } => {
                assert!(reason.contains("limit"), "{reason}");
            }
            other => panic!("expected InvalidCursor, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn u64_offset_in_cursor_does_not_overflow() {
        // u32::MAX + 1 — would overflow a u32-based cursor.
        let server = MockServer::start().await;
        let huge = u64::from(u32::MAX) + 1;
        Mock::given(method("GET"))
            .and(path("/api/v2/rest/dataset"))
            .and(query_param("offset", huge.to_string()))
            .and(query_param("limit", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(sample_envelope(0, &[])))
            .mount(&server)
            .await;

        let page = connector(&server)
            .list_datasets(Some(Cursor::new(format!("{huge}:2"))))
            .await
            .expect("ok");
        assert!(page.items.is_empty());
    }

    #[test]
    fn parse_cursor_defaults_to_zero_offset_and_provided_limit() {
        let (offset, limit) = parse_cursor(None, 100).expect("ok");
        assert_eq!(offset, 0);
        assert_eq!(limit, 100);
    }

    /// Regression for Copilot PR #94 round 3: `Some("")` in a CKAN
    /// optional-string field used to short-circuit `Option::or` chains
    /// and drop the value entirely (or surface as an empty string in
    /// the output). The `non_empty` helper now normalises empties to
    /// `None` so the fallback ladder keeps running.
    #[tokio::test]
    async fn empty_strings_fall_back_to_next_option() {
        let dataset = json!({
            "id": "regress",
            "name": "regress-slug",
            "title": "regress-zh",
            "license_title": "",
            "license_id": "CC-BY-4.0",
            "organization": {"name": "the-agency", "title": ""},
            "groups": [
                {"name": "real-name", "title": "", "display_name": ""},
                {"name": "fallback-name", "title": "", "display_name": "Fallback Display"}
            ],
            "frequency": ""
        });
        let envelope = sample_envelope(1, &[dataset]);
        let server = mock_server_with_pages(envelope, None).await;
        let d = connector(&server)
            .list_datasets(None)
            .await
            .unwrap()
            .items
            .remove(0);

        // license: empty title → fall through to license_id.
        assert_eq!(d.license, "CC-BY-4.0");
        // publisher: empty title → fall through to organization.name.
        assert_eq!(d.publisher.as_deref(), Some("the-agency"));
        // groups: each entry should pick the first NON-EMPTY label.
        assert_eq!(d.upstream_categories, vec!["real-name", "Fallback Display"]);
        // frequency: empty → None (not Some("")).
        assert!(d.update_frequency.is_none());
    }

    #[test]
    fn parse_cursor_rejects_negative_or_non_numeric() {
        for raw in ["abc:10", "-1:10", "10:-1", "10:abc", ":", "10:"] {
            let err = parse_cursor(Some(&Cursor::new(raw)), 100).unwrap_err();
            assert!(
                matches!(err, crate::ConnectorError::InvalidCursor { .. }),
                "{raw} should be rejected",
            );
        }
    }

    #[test]
    fn ckan_timestamp_parses_iso_with_and_without_zone() {
        let with_z = parse_ckan_timestamp("2026-04-15T03:30:00Z").unwrap();
        let without_z = parse_ckan_timestamp("2026-04-15T03:30:00").unwrap();
        let fractional = parse_ckan_timestamp("2026-04-15T03:30:00.123").unwrap();
        assert_eq!(with_z, without_z);
        assert_eq!(fractional.date_naive(), with_z.date_naive());
        assert!(parse_ckan_timestamp("not a date").is_none());
    }
}
