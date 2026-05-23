//! Fishery (MOA) connector (#5b.5).
//!
//! 農業部開放資料 (Ministry of Agriculture open-data hub) at
//! <https://data.moa.gov.tw> surfaces datasets from across MOA's
//! agencies, including 漁業署 (Fisheries Agency). The catalog
//! walk emits two fixed [`DatasetMetadata`] rows for the fishery
//! feeds Taiwan Data Hub cares about today:
//!
//! - **漁產品交易行情** (fish-product market transactions) —
//!   daily wholesale prices and traded volumes from the major
//!   fishery markets.
//! - **漁港進出統計** (fishing-port traffic statistics) —
//!   per-port vessel entry / exit counts.
//!
//! Both carry `upstream_categories = ["漁業"]` so the domain
//! mapper's substring match routes them into the
//! `agriculture-fisheries` domain (zh-TW name `農林漁牧` —
//! "漁" is the substring that matches).
//!
//! [`FisheryMoaConnector::list_datasets`] returns those two
//! rows verbatim. The per-dataset HTTP pulls land in a follow-
//! up via [`SourceConnector::fetch_data`]; the polite-GET
//! scaffolding ([`FisheryMoaConnector::polite_get`]) is here
//! so wiring will be a one-liner.
//!
//! ## Cross-cutting policies
//!
//! Same shape as the MOEA connector (no API key required, so
//! simpler than CWA):
//!
//! - **robots.txt respect** — Builder fetches `<base>/robots.txt`
//!   at construction (RFC 9309: §2.1 origin scoping, §2.2 blank-
//!   line group termination, §2.2.1 multi-agent groups,
//!   case-insensitive directive names). Disallowed paths
//!   produce [`ConnectorError::Config`].
//! - **Per-page throttle** — async-safe minimum interval,
//!   slot-based so concurrent callers don't serialise on the
//!   mutex across the sleep.
//! - **Defence-in-depth on `polite_get`** — path validator
//!   rejects absolute / scheme-relative / no-leading-slash
//!   inputs; post-join origin equality check; robots check
//!   uses the parsed `url.path()`; redirects disabled at the
//!   `Client` builder.
//!
//! The robots / throttle / path-validator scaffolding here
//! duplicates MOEA's (and CWA's, sans the key bits).
//! Intentional — see the matching note in
//! `crates/connectors/src/moea.rs`. With four connectors now
//! sharing this shape, `connectors::polite` extraction is the
//! right next move; flagged for a follow-up so this PR stays
//! scoped to the Fishery feed.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use tokio::sync::Mutex;
use tokio::time::Instant;
use url::Url;

use crate::{
    ConditionalCues, Cursor, DatasetMetadata, ListResponse, Page, SourceConnector, SourceId,
};

const DEFAULT_BASE_URL: &str = "https://data.moa.gov.tw";
const DEFAULT_TIMEOUT_SECS: u64 = 60;
/// Conservative minimum gap between upstream requests.
/// Matches the TWSE / MOEA / CWA connectors so operators
/// have one number to reason about.
const DEFAULT_THROTTLE_MS: u64 = 1000;
const USER_AGENT: &str = concat!(
    "taiwan-data-hub/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/hydai/taiwan-data-hub)"
);

/// The two fishery feeds the catalog walk emits. The string
/// values become the row's `datasets.source_id` AND its
/// `slug` (see the per-feed `*_metadata` helpers below), so
/// they need to be stable across releases. The
/// `source_id == slug` equality is intentional today; if the
/// upstream renames its path the fork happens at the constant
/// rather than at the call site.
const DATASET_ID_TRANSACTIONS: &str = "fishery-moa-transactions";
const DATASET_ID_PORT_TRAFFIC: &str = "fishery-moa-port-traffic";

/// HTTP client for the MOA open-data hub (fishery scope).
/// `Clone` so the worker's per-source cron-job closure can
/// capture an owned copy.
#[derive(Debug, Clone)]
pub struct FisheryMoaConnector {
    http: Client,
    base_url: Url,
    throttle: RequestThrottle,
    /// `robots.txt` disallow paths for the configured base
    /// URL's origin. MOA today serves a single host, so a
    /// flat `Vec` suffices.
    robots_disallowed: Arc<Vec<String>>,
}

impl FisheryMoaConnector {
    /// Construct a connector with production defaults — real
    /// MOA host, 1s throttle, robots.txt fetched from upstream.
    /// Use [`Self::builder`] to point at a wiremock server or
    /// tweak the throttle for tests.
    ///
    /// Performs ONE HTTP call (robots.txt) before returning.
    pub async fn new() -> Result<Self, BuildError> {
        Self::builder().build().await
    }

    #[must_use]
    pub fn builder() -> Builder {
        Builder::default()
    }

    /// Is `path` permitted by the cached robots.txt disallow
    /// list? `path` is the URL path component.
    #[must_use]
    pub fn path_allowed(&self, path: &str) -> bool {
        !self
            .robots_disallowed
            .iter()
            .any(|prefix| path.starts_with(prefix.as_str()))
    }

    /// For tests: snapshot of the disallow list.
    #[cfg(test)]
    pub(crate) fn robots_disallowed(&self) -> &[String] {
        self.robots_disallowed.as_slice()
    }

    /// Polite GET against the MOA host — sleeps on the
    /// throttle, joins the path against the configured base,
    /// refuses disallowed paths, and issues the request.
    /// Wraps the per-request policy the future `fetch_data`
    /// impl will reuse for the per-dataset pulls; exposing
    /// it now also keeps the stored http / base-url /
    /// throttle fields exercised in the catalog-only build
    /// (no `dead_code` allow needed).
    pub async fn polite_get(&self, path: &str) -> Result<reqwest::Response, crate::ConnectorError> {
        // Reject anything that isn't a same-origin relative
        // path. `Url::join` would otherwise accept an
        // absolute URL or scheme-relative URL and silently
        // swap the origin, bypassing the same-origin and
        // robots-prefix checks.
        validate_relative_path(path)?;
        let url = self
            .base_url
            .join(path)
            .map_err(|e| crate::ConnectorError::Config(format!("invalid path {path:?}: {e}")))?;
        // Belt + suspenders: even if `validate_relative_path`
        // someday admits a corner case, refuse the request
        // when `Url::join` produced a different origin than
        // the configured base.
        if url.origin() != self.base_url.origin() {
            return Err(crate::ConnectorError::Config(format!(
                "path {path:?} resolved to a different origin than {}",
                origin_key(&self.base_url),
            )));
        }
        // Use the PARSED url's path for the robots check —
        // an attacker-controlled `path` could carry tricks
        // like `/foo/../private/` that `Url::join`
        // normalises. Checking the normalised form matches
        // what the upstream server will actually see.
        if !self.path_allowed(url.path()) {
            return Err(crate::ConnectorError::Config(format!(
                "path {:?} disallowed by robots.txt",
                url.path(),
            )));
        }
        self.throttle.tick().await;
        let resp = self.http.get(url).send().await?;
        let status = resp.status();
        if status.is_success() {
            Ok(resp)
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(crate::ConnectorError::BadStatus {
                status: status.as_u16(),
                body,
            })
        }
    }
}

/// Reject anything that would let `Url::join` swap the
/// origin out from under the same-origin contract.
fn validate_relative_path(path: &str) -> Result<(), crate::ConnectorError> {
    if !path.starts_with('/') {
        return Err(crate::ConnectorError::Config(format!(
            "path {path:?} must start with '/' (got a non-relative path)",
        )));
    }
    if path.starts_with("//") {
        return Err(crate::ConnectorError::Config(format!(
            "path {path:?} must not start with '//' (scheme-relative URLs are forbidden)",
        )));
    }
    if path.contains("://") {
        return Err(crate::ConnectorError::Config(format!(
            "path {path:?} must not contain '://' (absolute URLs are forbidden)",
        )));
    }
    Ok(())
}

#[async_trait]
impl SourceConnector for FisheryMoaConnector {
    fn source_id(&self) -> SourceId {
        SourceId::FisheryMoa
    }

    async fn list_datasets(
        &self,
        _cursor: Option<Cursor>,
        _cues: &ConditionalCues,
    ) -> Result<ListResponse, crate::ConnectorError> {
        // MOA's open-data hub has searchable categories but
        // no single "list all fishery datasets" endpoint —
        // the two known feeds are returned verbatim.
        // ConditionalCues are ignored because there's no
        // upstream ETag for a synthetic catalog; subsequent
        // runs emit the same rows and the ETL upsert layer
        // (driver checksum check) will skip-without-
        // rewriting when nothing changed.
        let items = vec![transactions_metadata(), port_traffic_metadata()];
        let total = u64::try_from(items.len()).unwrap_or(u64::MAX);
        Ok(ListResponse::Modified {
            page: Page {
                items,
                next: None,
                total: Some(total),
            },
            fresh_cues: ConditionalCues::default(),
        })
    }

    fn supports_incremental(&self) -> bool {
        // Flip to `true` once `fetch_data` is implemented.
        false
    }
}

fn transactions_metadata() -> DatasetMetadata {
    let mut title = BTreeMap::new();
    title.insert("zh-TW".into(), "漁產品交易行情".into());
    title.insert(
        "en".into(),
        "MOA Fishery Product Market Transactions".into(),
    );
    let mut description = BTreeMap::new();
    description.insert(
        "zh-TW".into(),
        "全國主要漁市場每日漁產品交易資料,含品名、交易量、上中下價及平均價。".into(),
    );
    description.insert(
        "en".into(),
        "Daily transaction records from Taiwan's major fishery markets: species, \
         traded volume, high/mid/low prices, and average price."
            .into(),
    );
    DatasetMetadata {
        source_id: DATASET_ID_TRANSACTIONS.into(),
        slug: DATASET_ID_TRANSACTIONS.into(),
        title_i18n: title,
        description_i18n: description,
        license: "OGDL-Taiwan-1.0".into(),
        publisher: Some("農業部漁業署".into()),
        update_frequency: Some("daily".into()),
        original_url: Some("https://data.moa.gov.tw/open_data_detail.aspx?id=42".into()),
        last_modified_at: None,
        // The domain mapper's substring match: "漁業"
        // contains "漁", which is also in the
        // agriculture-fisheries domain's zh-TW name
        // "農林漁牧". Both fishery feeds route there.
        upstream_categories: vec!["漁業".into()],
    }
}

fn port_traffic_metadata() -> DatasetMetadata {
    let mut title = BTreeMap::new();
    title.insert("zh-TW".into(), "漁港進出統計".into());
    title.insert("en".into(), "MOA Fishing Port Entry/Exit Statistics".into());
    let mut description = BTreeMap::new();
    description.insert(
        "zh-TW".into(),
        "各漁港船舶進出港統計資料,含進港數、出港數、漁船別等。".into(),
    );
    description.insert(
        "en".into(),
        "Vessel entry / exit counts per fishing port, broken down by vessel category.".into(),
    );
    DatasetMetadata {
        source_id: DATASET_ID_PORT_TRAFFIC.into(),
        slug: DATASET_ID_PORT_TRAFFIC.into(),
        title_i18n: title,
        description_i18n: description,
        license: "OGDL-Taiwan-1.0".into(),
        publisher: Some("農業部漁業署".into()),
        update_frequency: Some("daily".into()),
        original_url: Some("https://data.moa.gov.tw/open_data_detail.aspx?id=82".into()),
        last_modified_at: None,
        upstream_categories: vec!["漁業".into()],
    }
}

/// Async-safe minimum-interval throttle. Same shape as
/// MOEA / CWA — slot-based so concurrent callers each get
/// a distinct reservation rather than serialising on the
/// mutex across the sleep.
#[derive(Debug, Clone)]
struct RequestThrottle {
    next_allowed_at: Arc<Mutex<Option<Instant>>>,
    min_interval: Duration,
}

impl RequestThrottle {
    fn new(min_interval: Duration) -> Self {
        Self {
            next_allowed_at: Arc::new(Mutex::new(None)),
            min_interval,
        }
    }

    async fn tick(&self) {
        let deadline = {
            let mut guard = self.next_allowed_at.lock().await;
            let now = Instant::now();
            let deadline = match *guard {
                None => now,
                Some(prior) => prior.max(now),
            };
            *guard = Some(deadline + self.min_interval);
            deadline
        };
        tokio::time::sleep_until(deadline).await;
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// `which` names the configuration setting that held
    /// the bad value (e.g. `base_url`). `value` carries the
    /// offending string verbatim. The underlying
    /// `url::ParseError` is preserved via `#[source]` for
    /// chain walkers.
    #[error("invalid {which} URL {value:?}")]
    InvalidUrl {
        which: &'static str,
        value: String,
        #[source]
        source: url::ParseError,
    },
    #[error("HTTP client could not be constructed: {0}")]
    Client(#[from] reqwest::Error),
    #[error("robots.txt fetch from {url} failed: {source}")]
    RobotsFetch {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("robots.txt fetch from {url} returned HTTP {status}")]
    RobotsStatus { url: String, status: u16 },
}

/// Builder for [`FisheryMoaConnector`]. The auto-fetch-
/// robots behaviour is opt-out: production wants robots.txt
/// honoured, but tests pointing at wiremock don't want the
/// connector to try the real MOA host.
#[derive(Debug, Clone)]
pub struct Builder {
    base_url: String,
    timeout_secs: u64,
    throttle_ms: u64,
    auto_fetch_robots: bool,
}

impl Default for Builder {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            throttle_ms: DEFAULT_THROTTLE_MS,
            auto_fetch_robots: true,
        }
    }
}

impl Builder {
    #[must_use]
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    #[must_use]
    pub fn timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    #[must_use]
    pub fn throttle_ms(mut self, ms: u64) -> Self {
        self.throttle_ms = ms;
        self
    }

    #[must_use]
    pub fn auto_fetch_robots(mut self, on: bool) -> Self {
        self.auto_fetch_robots = on;
        self
    }

    pub async fn build(self) -> Result<FisheryMoaConnector, BuildError> {
        let base_url = Url::parse(&self.base_url).map_err(|e| BuildError::InvalidUrl {
            which: "base_url",
            value: self.base_url.clone(),
            source: e,
        })?;
        // Disable HTTP redirects so the same-origin and
        // robots-prefix checks above stay authoritative.
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(self.timeout_secs))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let throttle = RequestThrottle::new(Duration::from_millis(self.throttle_ms));
        let robots_disallowed = if self.auto_fetch_robots {
            fetch_robots_disallowed(&http, &base_url, &throttle).await?
        } else {
            Vec::new()
        };
        Ok(FisheryMoaConnector {
            http,
            base_url,
            throttle,
            robots_disallowed: Arc::new(robots_disallowed),
        })
    }
}

async fn fetch_robots_disallowed(
    http: &Client,
    base: &Url,
    throttle: &RequestThrottle,
) -> Result<Vec<String>, BuildError> {
    let url = base
        .join("/robots.txt")
        .map_err(|e| BuildError::InvalidUrl {
            which: "robots.txt URL",
            value: format!("{base}/robots.txt"),
            source: e,
        })?;
    throttle.tick().await;
    let url_str = url.to_string();
    let response = http
        .get(url.clone())
        .send()
        .await
        .map_err(|e| BuildError::RobotsFetch {
            url: url_str.clone(),
            source: e,
        })?;
    let status = response.status();
    if status.as_u16() == 404 {
        tracing::info!(robots_url = %url_str, "robots.txt 404 — treating as permissive");
        return Ok(Vec::new());
    }
    if !status.is_success() {
        return Err(BuildError::RobotsStatus {
            url: url_str,
            status: status.as_u16(),
        });
    }
    let body = response.text().await.map_err(|e| BuildError::RobotsFetch {
        url: url_str,
        source: e,
    })?;
    Ok(parse_user_agent_star_disallow(&body))
}

fn origin_key(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// Pull `Disallow:` lines under any `User-agent: *` group.
/// Carries the RFC 9309 §2.2 blank-line group-termination
/// fix from #5b.3 so an empty `*` group can't leak `*`
/// into the next group's membership.
fn parse_user_agent_star_disallow(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current_agents: Vec<String> = Vec::new();
    let mut collecting_rules = false;
    for raw_line in body.lines() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            // §2.2: blank line ends the current group.
            current_agents.clear();
            collecting_rules = false;
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key_lc = key.trim().to_ascii_lowercase();
        let value = value.trim();
        if key_lc == "user-agent" {
            if collecting_rules {
                current_agents.clear();
                collecting_rules = false;
            }
            if !value.is_empty() {
                current_agents.push(value.to_string());
            }
            continue;
        }
        let group_has_star = current_agents.iter().any(|a| a == "*");
        if matches!(key_lc.as_str(), "disallow" | "allow") {
            collecting_rules = true;
            if !group_has_star {
                continue;
            }
            if key_lc == "disallow" && !value.is_empty() {
                out.push(value.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn skeleton_connector() -> FisheryMoaConnector {
        FisheryMoaConnector {
            http: Client::new(),
            base_url: Url::parse(DEFAULT_BASE_URL).unwrap(),
            throttle: RequestThrottle::new(Duration::from_millis(1)),
            robots_disallowed: Arc::new(Vec::new()),
        }
    }

    #[test]
    fn source_id_is_fishery_moa() {
        assert_eq!(skeleton_connector().source_id(), SourceId::FisheryMoa);
    }

    #[test]
    fn transactions_metadata_routes_to_agriculture_fisheries_domain() {
        let d = transactions_metadata();
        assert_eq!(d.source_id, DATASET_ID_TRANSACTIONS);
        assert_eq!(d.slug, DATASET_ID_TRANSACTIONS);
        assert_eq!(d.upstream_categories, vec!["漁業"]);
        assert!(d.title_i18n.contains_key("zh-TW"));
        assert!(d.title_i18n.contains_key("en"));
    }

    #[test]
    fn port_traffic_metadata_routes_to_agriculture_fisheries_domain() {
        let d = port_traffic_metadata();
        assert_eq!(d.source_id, DATASET_ID_PORT_TRAFFIC);
        assert_eq!(d.slug, DATASET_ID_PORT_TRAFFIC);
        assert_eq!(d.upstream_categories, vec!["漁業"]);
    }

    #[tokio::test]
    async fn list_datasets_returns_two_fixed_rows() {
        let connector = FisheryMoaConnector::builder()
            .base_url("https://example.test")
            .auto_fetch_robots(false)
            .build()
            .await
            .unwrap();
        let resp = connector
            .list_datasets(None, &ConditionalCues::default())
            .await
            .unwrap();
        let ListResponse::Modified { page, .. } = resp else {
            panic!("expected Modified");
        };
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.total, Some(2));
        let source_ids: Vec<_> = page.items.iter().map(|d| d.source_id.as_str()).collect();
        assert_eq!(
            source_ids,
            vec![DATASET_ID_TRANSACTIONS, DATASET_ID_PORT_TRAFFIC]
        );
    }

    #[tokio::test]
    async fn supports_incremental_is_false_today() {
        let connector = FisheryMoaConnector::builder()
            .base_url("https://example.test")
            .auto_fetch_robots(false)
            .build()
            .await
            .unwrap();
        assert!(!connector.supports_incremental());
    }

    #[test]
    fn validate_relative_path_accepts_canonical_paths() {
        assert!(validate_relative_path("/open_data_detail.aspx").is_ok());
        assert!(validate_relative_path("/").is_ok());
    }

    #[test]
    fn validate_relative_path_rejects_absolute_url() {
        let err = validate_relative_path("https://evil.example/x").unwrap_err();
        assert!(
            matches!(&err, crate::ConnectorError::Config(msg) if msg.contains("://")),
            "got {err:?}",
        );
    }

    #[test]
    fn validate_relative_path_rejects_scheme_relative_url() {
        let err = validate_relative_path("//evil.example/x").unwrap_err();
        assert!(
            matches!(&err, crate::ConnectorError::Config(msg) if msg.contains("//")),
            "got {err:?}",
        );
    }

    #[test]
    fn validate_relative_path_rejects_relative_without_slash() {
        let err = validate_relative_path("open_data_detail.aspx").unwrap_err();
        assert!(
            matches!(&err, crate::ConnectorError::Config(msg) if msg.contains("start with '/'")),
            "got {err:?}",
        );
        assert!(validate_relative_path("").is_err());
    }

    #[tokio::test]
    async fn polite_get_rejects_absolute_url_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let connector = FisheryMoaConnector::builder()
            .base_url(server.uri())
            .throttle_ms(10)
            .auto_fetch_robots(false)
            .build()
            .await
            .unwrap();
        let err = connector
            .polite_get("https://evil.example/x")
            .await
            .expect_err("absolute URL must be rejected");
        assert!(
            matches!(&err, crate::ConnectorError::Config(msg) if msg.contains("://")),
            "got {err:?}",
        );
    }

    #[tokio::test]
    async fn polite_get_does_not_follow_redirects() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/redirect-me"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("Location", "https://evil.example/owned"),
            )
            .mount(&server)
            .await;
        let connector = FisheryMoaConnector::builder()
            .base_url(server.uri())
            .throttle_ms(10)
            .auto_fetch_robots(false)
            .build()
            .await
            .unwrap();
        let err = connector
            .polite_get("/redirect-me")
            .await
            .expect_err("3xx must surface as BadStatus, not be followed");
        assert!(
            matches!(&err, crate::ConnectorError::BadStatus { status: 302, .. }),
            "got {err:?}",
        );
    }

    #[tokio::test]
    async fn polite_get_refuses_disallowed_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/robots.txt"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("User-agent: *\nDisallow: /admin\n"),
            )
            .mount(&server)
            .await;
        let connector = FisheryMoaConnector::builder()
            .base_url(server.uri())
            .throttle_ms(10)
            .build()
            .await
            .unwrap();
        let err = connector
            .polite_get("/admin/secret")
            .await
            .expect_err("disallowed path must be rejected");
        assert!(
            matches!(&err, crate::ConnectorError::Config(msg) if msg.contains("disallowed by robots.txt")),
            "got {err:?}",
        );
    }

    #[tokio::test]
    async fn build_treats_robots_404_as_permissive() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/robots.txt"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let connector = FisheryMoaConnector::builder()
            .base_url(server.uri())
            .throttle_ms(10)
            .build()
            .await
            .unwrap();
        assert!(connector.robots_disallowed().is_empty());
    }

    #[tokio::test]
    async fn build_fails_loudly_on_robots_5xx() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/robots.txt"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let err = FisheryMoaConnector::builder()
            .base_url(server.uri())
            .throttle_ms(10)
            .build()
            .await
            .expect_err("503 should fail");
        assert!(matches!(err, BuildError::RobotsStatus { status: 503, .. }));
    }

    #[tokio::test]
    async fn build_error_invalid_url_carries_input_value() {
        let err = FisheryMoaConnector::builder()
            .base_url("not a url")
            .auto_fetch_robots(false)
            .build()
            .await
            .expect_err("malformed base_url must fail");
        match &err {
            BuildError::InvalidUrl { which, value, .. } => {
                assert_eq!(*which, "base_url");
                assert_eq!(value, "not a url");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn origin_key_includes_scheme_and_port() {
        let a = origin_key(&Url::parse("http://example.test:8001/foo").unwrap());
        let b = origin_key(&Url::parse("http://example.test:8002/foo").unwrap());
        assert_ne!(a, b);
    }

    #[test]
    fn robots_parser_extracts_star_disallow() {
        let body = "User-agent: *\nDisallow: /admin\n";
        assert_eq!(parse_user_agent_star_disallow(body), vec!["/admin"]);
    }

    #[test]
    fn robots_parser_ignores_other_agents() {
        let body = "User-agent: GoogleBot\nDisallow: /google\nUser-agent: *\nDisallow: /star\n";
        let out = parse_user_agent_star_disallow(body);
        assert_eq!(out, vec!["/star".to_string()]);
    }

    #[test]
    fn robots_parser_handles_comments_and_leading_blank_lines() {
        // Leading blank lines (and inline `#` comments) are
        // ignored. Both Disallow lines stay in the same
        // `*` group because no blank line separates them.
        let body = "\
# leading comment\n\
\n\
User-agent: *\n\
Disallow: /x  # inline comment\n\
Disallow: /y\n";
        let out = parse_user_agent_star_disallow(body);
        assert_eq!(out, vec!["/x".to_string(), "/y".to_string()]);
    }

    #[test]
    fn robots_parser_blank_line_terminates_group() {
        // RFC 9309 §2.2 — empty `*` group + blank line must
        // NOT leak `*` into the next group's membership.
        let body = "User-agent: *\n\nUser-agent: GoogleBot\nDisallow: /private\n";
        assert!(parse_user_agent_star_disallow(body).is_empty());
    }

    #[test]
    fn robots_parser_blank_line_after_rules_terminates_group() {
        // Same termination semantics when the `*` group
        // has rules: the blank line ends the group so
        // subsequent GoogleBot rules are not collected.
        let body = "\
User-agent: *\n\
Disallow: /first\n\
\n\
User-agent: GoogleBot\n\
Disallow: /second\n";
        let out = parse_user_agent_star_disallow(body);
        assert_eq!(out, vec!["/first".to_string()]);
    }

    #[test]
    fn robots_parser_handles_multi_user_agent_group() {
        // RFC 9309 §2.2.1: a single group may list multiple
        // `User-agent:` lines before its rules. `* + AdsBot`
        // means the star group catches the rule.
        let body = "User-agent: *\nUser-agent: AdsBot\nDisallow: /shared\n";
        let out = parse_user_agent_star_disallow(body);
        assert_eq!(out, vec!["/shared".to_string()]);
    }

    #[test]
    fn robots_parser_starts_new_group_after_rules() {
        // Once we've seen a Disallow, a subsequent
        // User-agent starts a NEW group — so the AdsBot
        // group below shouldn't inherit the * group's
        // membership.
        let body = "\
User-agent: *\n\
Disallow: /star-only\n\
User-agent: AdsBot\n\
Disallow: /adsbot-only\n";
        let out = parse_user_agent_star_disallow(body);
        assert_eq!(out, vec!["/star-only".to_string()]);
    }

    #[test]
    fn robots_parser_is_case_insensitive_on_directive_names() {
        let body = "USER-AGENT: *\nDISALLOW: /upper\nDisAllow: /mixed\n";
        let out = parse_user_agent_star_disallow(body);
        assert_eq!(out, vec!["/upper".to_string(), "/mixed".to_string()]);
    }

    #[test]
    fn robots_parser_skips_empty_disallow_directive() {
        // RFC 9309 §2.2.2: an empty Disallow means "no
        // restrictions" for this agent. We model that by
        // simply not emitting a prefix.
        let body = "User-agent: *\nDisallow:\n";
        let out = parse_user_agent_star_disallow(body);
        assert!(out.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn throttle_enforces_minimum_interval_between_ticks() {
        let throttle = RequestThrottle::new(Duration::from_millis(50));
        throttle.tick().await;
        let start = Instant::now();
        throttle.tick().await;
        let elapsed = Instant::now() - start;
        assert_eq!(elapsed, Duration::from_millis(50));
    }
}
