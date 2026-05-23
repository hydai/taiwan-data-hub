//! TWSE (Taiwan Stock Exchange) + MOPS connector (#5b.2).
//!
//! TWSE's open data isn't a CKAN-style catalog — there's no
//! `/api/v2/rest/dataset` to enumerate. Three well-known feeds
//! are exposed under the TWSE / MOPS subdomains:
//!
//! - **上市公司日成交資訊** (daily trading info) — TWSE
//!   `/exchangeReport/STOCK_DAY`. JSON per-stock-per-month.
//! - **月營收** (monthly revenue) — MOPS
//!   `/mops/web/t05st10_ifrs`. HTML per-stock-per-month.
//! - **重大訊息** (major announcements) — MOPS
//!   `/mops/web/t05st02`. HTML disclosure feed.
//!
//! [`TwseConnector::list_datasets`] returns three fixed
//! [`DatasetMetadata`] rows representing these feeds — that's
//! the entire "catalog walk" for TWSE. Per-stock CSV fetches
//! land in a follow-up via [`SourceConnector::fetch_data`];
//! the default `Unsupported` impl keeps the surface honest
//! today.
//!
//! Two cross-cutting policies are encoded here per the
//! issue's Definition of Done:
//!
//! - **robots.txt respect** — at construction the builder
//!   fetches `<host>/robots.txt`, parses the `User-agent: *`
//!   disallow list, and stores it. Every outbound request
//!   (today: just the robots fetch itself; tomorrow: the
//!   per-stock fetches in `fetch_data`) consults the cached
//!   list via [`TwseConnector::path_allowed`]. A disallowed
//!   path produces [`ConnectorError::Config`] rather than a
//!   silent skip — the worker should DLQ a misconfig loudly.
//! - **per-page throttle** — a connector-wide minimum
//!   interval between upstream calls, gated by an async mutex
//!   on the last-request timestamp. The catalog walk doesn't
//!   issue HTTP (the three rows are static), so the throttle
//!   is exercised only on the robots.txt fetch + future
//!   `fetch_data` calls — but the infrastructure lives here
//!   now so the per-stock follow-up is a 1-line wiring.

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

const DEFAULT_TWSE_BASE_URL: &str = "https://www.twse.com.tw";
const DEFAULT_MOPS_BASE_URL: &str = "https://mops.twse.com.tw";
const DEFAULT_TIMEOUT_SECS: u64 = 60;
/// Conservative minimum gap between upstream requests. TWSE's
/// public guidance is fuzzy ("don't hammer"); 1 second is well
/// inside any reasonable interpretation and keeps us friendly
/// without slowing down the 3-row catalog walk meaningfully.
const DEFAULT_THROTTLE_MS: u64 = 1000;
const USER_AGENT: &str = concat!(
    "taiwan-data-hub/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/hydai/taiwan-data-hub)"
);

/// The three TWSE feeds the catalog walk emits. The string
/// values become the row's `datasets.source_id` column on
/// upsert, so they need to be stable across releases.
const DATASET_ID_DAILY_TRADES: &str = "twse-stock-day";
const DATASET_ID_MONTHLY_REVENUE: &str = "twse-monthly-revenue";
const DATASET_ID_MAJOR_NEWS: &str = "twse-major-news";

/// HTTP client for TWSE + MOPS. `Clone` so the worker's per-
/// source cron-job closure can capture an owned copy.
#[derive(Debug, Clone)]
pub struct TwseConnector {
    http: Client,
    twse_base_url: Url,
    mops_base_url: Url,
    throttle: RequestThrottle,
    /// Robots.txt disallow paths discovered at construction.
    /// Stored as a sorted list of path prefixes. Lookup via
    /// [`Self::path_allowed`] is a linear scan — fine for the
    /// tiny disallow lists TWSE / MOPS publish in practice.
    robots_disallowed: Arc<Vec<String>>,
}

impl TwseConnector {
    /// Construct a connector with production-leaning defaults
    /// (real TWSE / MOPS hosts, 1s throttle, robots.txt
    /// fetched from upstream). Use [`Self::builder`] to point
    /// at a wiremock server or tweak the throttle for tests.
    ///
    /// Performs ONE HTTP call (robots.txt) before returning —
    /// the builder accepts an `auto_fetch_robots = false`
    /// escape hatch for tests that don't want the network
    /// touch.
    pub async fn new() -> Result<Self, BuildError> {
        Self::builder().build().await
    }

    #[must_use]
    pub fn builder() -> Builder {
        Builder::default()
    }

    /// Is `path` permitted by the cached robots.txt disallow
    /// list? `path` is the URL path component (e.g.
    /// `/exchangeReport/STOCK_DAY`). The check is a simple
    /// prefix match against each disallow entry — matches
    /// the User-agent: * directive's semantics for the cases
    /// TWSE / MOPS publish.
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
        &self.robots_disallowed
    }

    /// For tests: trigger a throttle tick (so a test can
    /// assert min-interval enforcement without going through
    /// the full HTTP path).
    #[cfg(test)]
    pub(crate) async fn throttle_tick(&self) {
        self.throttle.tick().await;
    }

    /// Polite GET against the TWSE host — sleeps on the
    /// throttle, joins the path, refuses disallowed paths,
    /// and issues the request. Wraps the per-request policy
    /// the future `fetch_data` impl will reuse for the
    /// per-stock CSV pulls; exposing it now also keeps the
    /// stored http / base-url / throttle fields exercised in
    /// the catalog-only build (no `dead_code` allow needed).
    pub async fn polite_get_twse(
        &self,
        path: &str,
    ) -> Result<reqwest::Response, crate::ConnectorError> {
        self.polite_get(&self.twse_base_url, path).await
    }

    /// Polite GET against the MOPS host. Same policy as
    /// [`Self::polite_get_twse`] — see that method's doc.
    pub async fn polite_get_mops(
        &self,
        path: &str,
    ) -> Result<reqwest::Response, crate::ConnectorError> {
        self.polite_get(&self.mops_base_url, path).await
    }

    async fn polite_get(
        &self,
        base: &Url,
        path: &str,
    ) -> Result<reqwest::Response, crate::ConnectorError> {
        if !self.path_allowed(path) {
            return Err(crate::ConnectorError::Config(format!(
                "path {path:?} disallowed by robots.txt for {}",
                base.host_str().unwrap_or("(unknown host)"),
            )));
        }
        let url = base
            .join(path)
            .map_err(|e| crate::ConnectorError::Config(format!("invalid path {path:?}: {e}")))?;
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

#[async_trait]
impl SourceConnector for TwseConnector {
    fn source_id(&self) -> SourceId {
        SourceId::Twse
    }

    async fn list_datasets(
        &self,
        _cursor: Option<Cursor>,
        _cues: &ConditionalCues,
    ) -> Result<ListResponse, crate::ConnectorError> {
        // TWSE has no upstream catalog endpoint — the three
        // known feeds are returned verbatim. ConditionalCues
        // are ignored because there's no upstream ETag /
        // Last-Modified to consult; subsequent runs will
        // emit the same rows and the ETL upsert layer (driver
        // checksum check) will skip-without-rewriting when
        // nothing changed.
        let items = vec![
            daily_trades_metadata(),
            monthly_revenue_metadata(),
            major_news_metadata(),
        ];
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

    // `fetch_metadata` and `fetch_data` keep their trait
    // defaults (`ConnectorError::Unsupported`). The per-stock
    // fetches that would populate `fetch_data` are a follow-
    // up — see the module doc comment.

    fn supports_incremental(&self) -> bool {
        // Flip to `true` once `fetch_data` is implemented.
        // The driver consults this before invoking
        // per-dataset methods so the catalog-only path stays
        // the only reachable one today.
        false
    }
}

fn daily_trades_metadata() -> DatasetMetadata {
    let mut title = BTreeMap::new();
    title.insert("zh-TW".into(), "上市公司日成交資訊".into());
    title.insert("en".into(), "TWSE Listed Stocks Daily Trades".into());
    let mut description = BTreeMap::new();
    description.insert(
        "zh-TW".into(),
        "每日成交量、開盤價、收盤價、最高最低價,以個股為單位。".into(),
    );
    description.insert(
        "en".into(),
        "Per-stock daily trading volume, open/close/high/low.".into(),
    );
    DatasetMetadata {
        source_id: DATASET_ID_DAILY_TRADES.into(),
        slug: "twse-stock-day".into(),
        title_i18n: title,
        description_i18n: description,
        license: "OGDL-Taiwan-1.0".into(),
        publisher: Some("臺灣證券交易所".into()),
        update_frequency: Some("daily".into()),
        original_url: Some(
            "https://www.twse.com.tw/zh/page/trading/exchange/STOCK_DAY.html".into(),
        ),
        last_modified_at: None,
        // The domain mapper's substring match (in either
        // direction) routes "經濟" into the "economy-business"
        // domain (zh_tw name "經濟與產業"). All three TWSE
        // feeds land there.
        upstream_categories: vec!["經濟".into()],
    }
}

fn monthly_revenue_metadata() -> DatasetMetadata {
    let mut title = BTreeMap::new();
    title.insert("zh-TW".into(), "上市公司月營收".into());
    title.insert("en".into(), "TWSE Listed Companies Monthly Revenue".into());
    let mut description = BTreeMap::new();
    description.insert(
        "zh-TW".into(),
        "每月各上市公司營業收入,公開資訊觀測站(MOPS) 提供。".into(),
    );
    description.insert(
        "en".into(),
        "Monthly revenue per listed company, published via the MOPS portal.".into(),
    );
    DatasetMetadata {
        source_id: DATASET_ID_MONTHLY_REVENUE.into(),
        slug: "twse-monthly-revenue".into(),
        title_i18n: title,
        description_i18n: description,
        license: "OGDL-Taiwan-1.0".into(),
        publisher: Some("臺灣證券交易所".into()),
        update_frequency: Some("monthly".into()),
        original_url: Some("https://mops.twse.com.tw/mops/web/t05st10_ifrs".into()),
        last_modified_at: None,
        upstream_categories: vec!["經濟".into()],
    }
}

fn major_news_metadata() -> DatasetMetadata {
    let mut title = BTreeMap::new();
    title.insert("zh-TW".into(), "上市公司重大訊息".into());
    title.insert("en".into(), "TWSE Major Announcements".into());
    let mut description = BTreeMap::new();
    description.insert(
        "zh-TW".into(),
        "上市公司重大訊息揭露,公開資訊觀測站(MOPS) 提供。".into(),
    );
    description.insert(
        "en".into(),
        "Listed company material disclosures, published via the MOPS portal.".into(),
    );
    DatasetMetadata {
        source_id: DATASET_ID_MAJOR_NEWS.into(),
        slug: "twse-major-news".into(),
        title_i18n: title,
        description_i18n: description,
        license: "OGDL-Taiwan-1.0".into(),
        publisher: Some("臺灣證券交易所".into()),
        update_frequency: Some("as published".into()),
        original_url: Some("https://mops.twse.com.tw/mops/web/t05st02".into()),
        last_modified_at: None,
        upstream_categories: vec!["經濟".into()],
    }
}

/// Async-safe minimum-interval throttle. Wraps an `Option<Instant>`
/// representing the last-tick time; `tick()` sleeps until at
/// least `min_interval` has passed since that, then updates
/// the stored time. The mutex is `tokio::sync::Mutex` because
/// it's held across an `.await` (the sleep) — an `std::sync::Mutex`
/// would block the runtime.
#[derive(Debug, Clone)]
struct RequestThrottle {
    last_request: Arc<Mutex<Option<Instant>>>,
    min_interval: Duration,
}

impl RequestThrottle {
    fn new(min_interval: Duration) -> Self {
        Self {
            last_request: Arc::new(Mutex::new(None)),
            min_interval,
        }
    }

    /// Wait if necessary so at least `min_interval` has
    /// passed since the previous tick, then mark the current
    /// instant as "last".
    async fn tick(&self) {
        let mut guard = self.last_request.lock().await;
        if let Some(prev) = *guard {
            // `checked_sub` keeps clippy's
            // `unchecked-time-subtraction` lint happy. When
            // elapsed ≥ min_interval the subtraction would
            // underflow Duration — but the outer check is
            // exactly that case, so `checked_sub` always
            // returns `Some` on the branch we sleep on.
            if let Some(wait) = self.min_interval.checked_sub(prev.elapsed()) {
                tokio::time::sleep(wait).await;
            }
        }
        *guard = Some(Instant::now());
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
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

/// Builder for [`TwseConnector`]. The auto-fetch-robots
/// behaviour is opt-out: production wants robots.txt
/// honoured, but tests pointing at wiremock don't want the
/// connector to try the real TWSE host. Tests can either
/// stub a robots.txt route on wiremock OR pass
/// `auto_fetch_robots(false)` to skip the fetch entirely.
#[derive(Debug, Clone)]
pub struct Builder {
    twse_base_url: String,
    mops_base_url: String,
    timeout_secs: u64,
    throttle_ms: u64,
    /// When `false`, skip the robots.txt fetch at build
    /// time and treat ALL paths as allowed. Test-only.
    auto_fetch_robots: bool,
}

impl Default for Builder {
    fn default() -> Self {
        Self {
            twse_base_url: DEFAULT_TWSE_BASE_URL.to_owned(),
            mops_base_url: DEFAULT_MOPS_BASE_URL.to_owned(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            throttle_ms: DEFAULT_THROTTLE_MS,
            auto_fetch_robots: true,
        }
    }
}

impl Builder {
    #[must_use]
    pub fn twse_base_url(mut self, url: impl Into<String>) -> Self {
        self.twse_base_url = url.into();
        self
    }

    #[must_use]
    pub fn mops_base_url(mut self, url: impl Into<String>) -> Self {
        self.mops_base_url = url.into();
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

    pub async fn build(self) -> Result<TwseConnector, BuildError> {
        let twse_base_url =
            Url::parse(&self.twse_base_url).map_err(|e| BuildError::InvalidUrl(e.to_string()))?;
        let mops_base_url =
            Url::parse(&self.mops_base_url).map_err(|e| BuildError::InvalidUrl(e.to_string()))?;
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(self.timeout_secs))
            .build()?;
        let throttle = RequestThrottle::new(Duration::from_millis(self.throttle_ms));
        let robots_disallowed = if self.auto_fetch_robots {
            fetch_robots_disallowed(&http, &twse_base_url, &throttle).await?
        } else {
            Vec::new()
        };
        Ok(TwseConnector {
            http,
            twse_base_url,
            mops_base_url,
            throttle,
            robots_disallowed: Arc::new(robots_disallowed),
        })
    }
}

/// Fetch `<base>/robots.txt`, parse, and return the
/// disallow paths under `User-agent: *`. Errors are bubbled
/// rather than swallowed — a connector that can't read
/// robots.txt at boot shouldn't run crawls. Goes through
/// the throttle so even the bootstrap fetch is polite.
async fn fetch_robots_disallowed(
    http: &Client,
    base: &Url,
    throttle: &RequestThrottle,
) -> Result<Vec<String>, BuildError> {
    let url = base
        .join("/robots.txt")
        .map_err(|e| BuildError::InvalidUrl(e.to_string()))?;
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
    // 404 robots.txt means "no restrictions" per RFC 9309.
    // Anything else non-2xx is a fail-loud — we don't know
    // whether the upstream blocks us or just hiccupped.
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

/// Pull `Disallow: ...` lines that fall under the
/// `User-agent: *` group. Other agents are ignored — we
/// identify as our own user-agent string but the safest
/// default is to honour the `*` group (most servers ONLY
/// have a `*` group). RFC 9309 §2.2.1 says a missing `*`
/// group means "no rules apply", which is what an empty
/// Vec encodes.
fn parse_user_agent_star_disallow(body: &str) -> Vec<String> {
    let mut in_star = false;
    let mut out = Vec::new();
    for raw_line in body.lines() {
        // Strip trailing comments per RFC 9309.
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("User-agent:") {
            in_star = rest.trim() == "*";
            continue;
        }
        // Common alternate spelling.
        if let Some(rest) = line.strip_prefix("User-Agent:") {
            in_star = rest.trim() == "*";
            continue;
        }
        if !in_star {
            continue;
        }
        if let Some(rest) = line.strip_prefix("Disallow:") {
            let path = rest.trim();
            // Empty Disallow means "allow everything" — skip
            // rather than store an empty prefix that would
            // match every URL.
            if !path.is_empty() {
                out.push(path.to_string());
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

    #[tokio::test]
    async fn list_datasets_returns_three_fixed_feeds() {
        // Construction skips robots.txt fetch via the
        // builder escape hatch — tests aren't pointing at
        // a real TWSE host.
        let connector = TwseConnector::builder()
            .auto_fetch_robots(false)
            .build()
            .await
            .expect("build");
        let response = connector
            .list_datasets(None, &ConditionalCues::default())
            .await
            .expect("list_datasets");
        let page = match response {
            ListResponse::Modified { page, .. } => page,
            ListResponse::NotModified => panic!("expected Modified, got NotModified"),
        };
        assert_eq!(page.items.len(), 3);
        assert_eq!(page.total, Some(3));
        assert!(page.next.is_none());
        let ids: Vec<&str> = page.items.iter().map(|m| m.source_id.as_str()).collect();
        assert!(ids.contains(&DATASET_ID_DAILY_TRADES));
        assert!(ids.contains(&DATASET_ID_MONTHLY_REVENUE));
        assert!(ids.contains(&DATASET_ID_MAJOR_NEWS));
    }

    #[tokio::test]
    async fn list_datasets_metadata_has_zh_tw_and_en_titles() {
        // The system's i18n contract requires zh-TW (source
        // language) for every dataset; we also ship en. The
        // domain mapper's substring match consults the
        // titles only indirectly (via upstream_categories);
        // this test pins the wire shape.
        let connector = TwseConnector::builder()
            .auto_fetch_robots(false)
            .build()
            .await
            .unwrap();
        let response = connector
            .list_datasets(None, &ConditionalCues::default())
            .await
            .unwrap();
        let page = match response {
            ListResponse::Modified { page, .. } => page,
            ListResponse::NotModified => unreachable!(),
        };
        for m in &page.items {
            assert!(m.title_i18n.contains_key("zh-TW"), "missing zh-TW: {m:?}");
            assert!(m.title_i18n.contains_key("en"), "missing en: {m:?}");
            assert_eq!(m.license, "OGDL-Taiwan-1.0");
            assert_eq!(m.publisher.as_deref(), Some("臺灣證券交易所"));
            assert!(!m.upstream_categories.is_empty());
        }
    }

    #[tokio::test]
    async fn supports_incremental_is_false_today() {
        // Catalog-only path — when fetch_data lands, this
        // assertion flips in lockstep.
        let connector = TwseConnector::builder()
            .auto_fetch_robots(false)
            .build()
            .await
            .unwrap();
        assert!(!connector.supports_incremental());
    }

    #[test]
    fn robots_parser_extracts_star_disallow() {
        let body = "\
User-agent: *
Disallow: /scripts/
Disallow: /private/
Allow: /

User-agent: Googlebot
Disallow: /noindex/
";
        let out = parse_user_agent_star_disallow(body);
        assert_eq!(out, vec!["/scripts/", "/private/"]);
    }

    #[test]
    fn robots_parser_ignores_other_agents() {
        // The Googlebot section's disallow MUST NOT bleed
        // into the * group's list.
        let body = "\
User-agent: Googlebot
Disallow: /no-google/
";
        let out = parse_user_agent_star_disallow(body);
        assert!(out.is_empty(), "got {out:?}");
    }

    #[test]
    fn robots_parser_handles_comments_and_blank_lines() {
        let body = "\
# top comment
User-agent: *
# inline comment
Disallow: /foo/  # trailing comment

Disallow: /bar/
";
        let out = parse_user_agent_star_disallow(body);
        assert_eq!(out, vec!["/foo/", "/bar/"]);
    }

    #[test]
    fn robots_parser_skips_empty_disallow_directive() {
        // RFC 9309: empty Disallow means "allow everything".
        // We must NOT store the empty string or every URL
        // would `starts_with("")` and be rejected.
        let body = "\
User-agent: *
Disallow:
";
        let out = parse_user_agent_star_disallow(body);
        assert!(out.is_empty(), "got {out:?}");
    }

    #[tokio::test]
    async fn path_allowed_rejects_disallow_prefix() {
        let connector = TwseConnector::builder()
            .auto_fetch_robots(false)
            .build()
            .await
            .unwrap();
        // Manually inject a disallow list via the test
        // accessor's underlying field — done by re-building
        // with a stubbed robots.txt response below would be
        // cleaner, but this assertion is on the policy
        // function's shape.
        let disallowed = vec!["/private/".to_string()];
        let connector = TwseConnector {
            robots_disallowed: Arc::new(disallowed),
            ..connector
        };
        assert!(connector.path_allowed("/exchangeReport/STOCK_DAY"));
        assert!(!connector.path_allowed("/private/secret.html"));
        assert!(!connector.path_allowed("/private/"));
    }

    #[tokio::test]
    async fn build_fetches_robots_when_auto_fetch_is_on() {
        // Wiremock TWSE serving a robots.txt that disallows
        // `/private/`. The connector must store that prefix.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/robots.txt"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("User-agent: *\nDisallow: /private/\nDisallow: /forbidden/\n"),
            )
            .mount(&server)
            .await;
        let connector = TwseConnector::builder()
            .twse_base_url(server.uri())
            // Tighten the throttle so the test isn't slow.
            .throttle_ms(10)
            .build()
            .await
            .expect("build");
        let disallowed = connector.robots_disallowed();
        assert_eq!(disallowed.len(), 2);
        assert!(disallowed.iter().any(|p| p == "/private/"));
        assert!(disallowed.iter().any(|p| p == "/forbidden/"));
        // Sanity check the policy function on the
        // network-derived list.
        assert!(!connector.path_allowed("/private/foo"));
        assert!(connector.path_allowed("/exchangeReport/STOCK_DAY"));
    }

    #[tokio::test]
    async fn build_treats_robots_404_as_permissive() {
        // RFC 9309: a 404 means no rules apply.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/robots.txt"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let connector = TwseConnector::builder()
            .twse_base_url(server.uri())
            .throttle_ms(10)
            .build()
            .await
            .expect("build");
        assert!(connector.robots_disallowed().is_empty());
        assert!(connector.path_allowed("/anything"));
    }

    #[tokio::test]
    async fn build_fails_loudly_on_robots_5xx() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/robots.txt"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let err = TwseConnector::builder()
            .twse_base_url(server.uri())
            .throttle_ms(10)
            .build()
            .await
            .expect_err("503 should fail");
        assert!(matches!(err, BuildError::RobotsStatus { status: 503, .. }));
    }

    #[tokio::test]
    async fn throttle_enforces_minimum_interval_between_ticks() {
        // Two ticks back-to-back with a 50ms throttle: the
        // second must observe ≥ 45ms (a little slack for
        // scheduler overhead) since the first.
        let connector = TwseConnector::builder()
            .auto_fetch_robots(false)
            .throttle_ms(50)
            .build()
            .await
            .unwrap();
        let start = std::time::Instant::now();
        connector.throttle_tick().await;
        connector.throttle_tick().await;
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(45),
            "expected ≥ 45ms between two ticks, got {elapsed:?}",
        );
    }

    #[tokio::test]
    async fn throttle_does_not_block_first_call() {
        // The very first tick should return immediately —
        // there's no prior tick to space from.
        let connector = TwseConnector::builder()
            .auto_fetch_robots(false)
            .throttle_ms(10_000) // 10s — would be obvious if it blocked
            .build()
            .await
            .unwrap();
        let start = std::time::Instant::now();
        connector.throttle_tick().await;
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "first tick should be ~immediate, got {elapsed:?}",
        );
    }
}
