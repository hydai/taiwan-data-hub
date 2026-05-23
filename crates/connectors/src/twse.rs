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
//!   fetches `<origin>/robots.txt` from BOTH the TWSE
//!   origin AND the MOPS origin (RFC 9309 §2.1 scopes
//!   robots.txt to the origin = scheme + host + port,
//!   NOT just host). Each `User-agent: *` group is
//!   parsed (RFC 9309 §2.2.1 — multi-agent groups,
//!   case-insensitive directive names) and stored in a
//!   map keyed by [`origin_key`]'s ASCII serialisation.
//!   Every outbound request consults the cached list
//!   for the origin it's targeting via
//!   [`TwseConnector::path_allowed_for_origin`]. A
//!   disallowed path produces
//!   [`ConnectorError::Config`] rather than a silent
//!   skip — the worker should DLQ a misconfig loudly.
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
    /// Per-origin robots.txt disallow lists, keyed by the
    /// origin's ASCII serialisation (`scheme://host[:port]`
    /// per `Url::origin().ascii_serialization()`). RFC 9309
    /// §2.1 scopes robots.txt to the origin, NOT just the
    /// host — a host can serve different rules on
    /// different ports / schemes, so the cache key has to
    /// include all three. Order within each list reflects
    /// what robots.txt published; prefix matching doesn't
    /// care about order, but preserving insertion order
    /// helps debugging.
    robots_disallowed: Arc<BTreeMap<String, Vec<String>>>,
}

impl TwseConnector {
    /// Construct a connector with production-leaning defaults
    /// (real TWSE / MOPS hosts, 1s throttle, robots.txt
    /// fetched from upstream). Use [`Self::builder`] to point
    /// at a wiremock server or tweak the throttle for tests.
    ///
    /// Performs TWO HTTP calls (robots.txt for each host)
    /// before returning — the builder accepts an
    /// `auto_fetch_robots = false` escape hatch for tests
    /// that don't want the network touch.
    pub async fn new() -> Result<Self, BuildError> {
        Self::builder().build().await
    }

    #[must_use]
    pub fn builder() -> Builder {
        Builder::default()
    }

    /// Is `path` permitted by the cached robots.txt
    /// disallow list **for the given origin**? `path` is
    /// the URL path component (e.g.
    /// `/exchangeReport/STOCK_DAY`); `origin` is the
    /// ASCII-serialised origin
    /// (`scheme://host[:port]`) — derive via
    /// [`origin_key`] from any `Url`. The check is a
    /// simple prefix match against each disallow entry
    /// for the origin — matches the `User-agent: *`
    /// directive's semantics for the cases TWSE / MOPS
    /// publish. An unknown origin is treated as
    /// permissive; the caller has already chosen
    /// `polite_get_twse` vs `polite_get_mops` so this is
    /// a defensive default rather than a real fallback
    /// path.
    #[must_use]
    pub fn path_allowed_for_origin(&self, origin: &str, path: &str) -> bool {
        let Some(disallowed) = self.robots_disallowed.get(origin) else {
            return true;
        };
        !disallowed
            .iter()
            .any(|prefix| path.starts_with(prefix.as_str()))
    }

    /// For tests: snapshot of the disallow list for the
    /// given origin, or an empty slice if the origin
    /// wasn't fetched.
    #[cfg(test)]
    pub(crate) fn robots_disallowed_for_origin(&self, origin: &str) -> &[String] {
        self.robots_disallowed
            .get(origin)
            .map_or(&[][..], Vec::as_slice)
    }

    /// For tests: trigger a throttle tick (so a test can
    /// assert min-interval enforcement without going through
    /// the full HTTP path).
    #[cfg(test)]
    pub(crate) async fn throttle_tick(&self) {
        self.throttle.tick().await;
    }

    /// Polite GET against the TWSE host — sleeps on the
    /// throttle, joins the path, refuses disallowed paths
    /// (per TWSE's robots.txt), and issues the request.
    /// Wraps the per-request policy the future `fetch_data`
    /// impl will reuse for the per-stock CSV pulls; exposing
    /// it now also keeps the stored http / base-url /
    /// throttle fields exercised in the catalog-only build
    /// (no `dead_code` allow needed).
    pub async fn polite_get_twse(
        &self,
        path: &str,
    ) -> Result<reqwest::Response, crate::ConnectorError> {
        self.polite_get(&self.twse_base_url, path).await
    }

    /// Polite GET against the MOPS host (consulting MOPS's
    /// own robots.txt). Same policy as
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
        let origin = origin_key(base);
        if !self.path_allowed_for_origin(&origin, path) {
            return Err(crate::ConnectorError::Config(format!(
                "path {path:?} disallowed by robots.txt for {origin}",
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
        let mut robots_disallowed: BTreeMap<String, Vec<String>> = BTreeMap::new();
        if self.auto_fetch_robots {
            // Fetch robots.txt from EACH origin independently
            // — RFC 9309 §2.1 scopes robots.txt to the
            // origin (scheme + host + port). Skipping the
            // MOPS fetch would mean MOPS requests are
            // checked against TWSE's rules; a host-only
            // dedup key would collapse two different
            // wiremock servers on `127.0.0.1` with different
            // ports into one cache entry.
            for base in [&twse_base_url, &mops_base_url] {
                let origin = origin_key(base);
                // If the operator pointed both URLs at the
                // same origin (truly identical scheme +
                // host + port), only fetch once.
                if robots_disallowed.contains_key(&origin) {
                    continue;
                }
                let disallowed = fetch_robots_disallowed(&http, base, &throttle).await?;
                robots_disallowed.insert(origin, disallowed);
            }
        }
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

/// Origin cache key per RFC 9309 §2.1 — the ASCII
/// serialisation of `Url::origin()`, which is
/// `scheme://host[:port]` with default ports elided.
/// This is the key both the fetcher and the lookup use,
/// keeping cache writes and reads in lockstep so a host
/// serving different rules on different ports (or
/// schemes) can't collide.
fn origin_key(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// Pull `Disallow: ...` lines that fall under any group
/// whose `User-agent:` membership includes `*`. RFC 9309
/// §2.2.1 lets a single group list multiple `User-agent:`
/// lines before any of its rules (e.g. `User-agent: *`
/// followed by `User-agent: AdsBot` and THEN the Disallow
/// lines apply to BOTH). The parser is a small state
/// machine: collect agent names into the current group,
/// switch into "rule-collecting" mode on the first
/// `Disallow:` (or `Allow:`) line, and start a fresh group
/// on the next `User-agent:` after that. Empty Disallow
/// means "allow everything"; we skip those rather than
/// store an empty prefix that would match every URL.
fn parse_user_agent_star_disallow(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current_agents: Vec<String> = Vec::new();
    let mut collecting_rules = false;
    for raw_line in body.lines() {
        // Strip trailing comments per RFC 9309 §2.2.
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = match line.split_once(':') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => continue,
        };
        // Case-insensitive on the directive name per RFC 9309.
        let key_lc = key.to_ascii_lowercase();
        if key_lc == "user-agent" {
            // A new User-agent AFTER we've started collecting
            // rules ends the previous group and starts a new
            // one; before any rules, it just adds to the
            // current group's membership.
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
        // The first rule line locks in the group's
        // membership for subsequent rules in this group.
        if matches!(key_lc.as_str(), "disallow" | "allow") {
            collecting_rules = true;
            if !group_has_star {
                continue;
            }
            if key_lc == "disallow" && !value.is_empty() {
                out.push(value.to_string());
            }
            // `Allow:` lines are intentionally NOT modelled
            // — we'd need a longest-match resolver per RFC
            // 9309 §3, and TWSE / MOPS in practice publish
            // disallow-only files. Documenting the limit
            // here keeps the contract honest.
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
    async fn path_allowed_for_origin_uses_per_origin_rules() {
        // Per-origin map: TWSE origin disallows `/private/`,
        // MOPS origin disallows `/internal/`. Same host
        // (`example.test`) but different ports — the
        // origin key keeps them apart per RFC 9309 §2.1.
        let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
        map.insert(
            "https://example.test:8001".into(),
            vec!["/private/".to_string()],
        );
        map.insert(
            "https://example.test:8002".into(),
            vec!["/internal/".to_string()],
        );
        let connector = TwseConnector::builder()
            .auto_fetch_robots(false)
            .build()
            .await
            .unwrap();
        let connector = TwseConnector {
            robots_disallowed: Arc::new(map),
            ..connector
        };
        // TWSE origin's disallow applies to its own paths.
        assert!(
            !connector.path_allowed_for_origin("https://example.test:8001", "/private/secret",)
        );
        // Same path against MOPS origin → allowed.
        assert!(connector.path_allowed_for_origin("https://example.test:8002", "/private/secret",));
        // MOPS origin's own disallow.
        assert!(!connector.path_allowed_for_origin("https://example.test:8002", "/internal/x",));
        assert!(connector.path_allowed_for_origin("https://example.test:8001", "/internal/x",));
        // Unknown origin: permissive (defensive default).
        assert!(connector.path_allowed_for_origin("https://other.example", "/anything"));
    }

    #[tokio::test]
    async fn build_fetches_robots_per_origin_not_per_host() {
        // Two wiremocks on `127.0.0.1` with different ports
        // → two distinct origins. The builder MUST fetch
        // robots.txt from both and store each list under
        // its own origin key. A host-only dedup key would
        // collapse them into one entry — exactly the bug
        // Round 2 flagged.
        let twse_server = MockServer::start().await;
        let mops_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/robots.txt"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("User-agent: *\nDisallow: /twse-private/\n"),
            )
            .mount(&twse_server)
            .await;
        Mock::given(method("GET"))
            .and(path("/robots.txt"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("User-agent: *\nDisallow: /mops-private/\n"),
            )
            .mount(&mops_server)
            .await;
        let connector = TwseConnector::builder()
            .twse_base_url(twse_server.uri())
            .mops_base_url(mops_server.uri())
            .throttle_ms(10)
            .build()
            .await
            .expect("build");
        let twse_origin = origin_key(&Url::parse(&twse_server.uri()).unwrap());
        let mops_origin = origin_key(&Url::parse(&mops_server.uri()).unwrap());
        assert_ne!(
            twse_origin, mops_origin,
            "wiremock should produce distinct origins (different ports)",
        );
        let twse_list = connector.robots_disallowed_for_origin(&twse_origin);
        let mops_list = connector.robots_disallowed_for_origin(&mops_origin);
        assert!(
            twse_list.iter().any(|p| p == "/twse-private/"),
            "twse rule should be under twse origin {twse_origin:?}, got {twse_list:?}",
        );
        assert!(
            mops_list.iter().any(|p| p == "/mops-private/"),
            "mops rule should be under mops origin {mops_origin:?}, got {mops_list:?}",
        );
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
            .mops_base_url(server.uri())
            .throttle_ms(10)
            .build()
            .await
            .expect("build");
        let origin = origin_key(&Url::parse(&server.uri()).unwrap());
        assert!(connector.robots_disallowed_for_origin(&origin).is_empty());
        assert!(connector.path_allowed_for_origin(&origin, "/anything"));
    }

    #[test]
    fn origin_key_includes_scheme_and_port() {
        // Origin key per RFC 9309 §2.1 must include all
        // three components so a host serving different
        // rules on different ports / schemes can't
        // collide.
        let a = origin_key(&Url::parse("http://example.test:8001/foo").unwrap());
        let b = origin_key(&Url::parse("http://example.test:8002/foo").unwrap());
        let c = origin_key(&Url::parse("https://example.test/foo").unwrap());
        let d = origin_key(&Url::parse("http://example.test/foo").unwrap());
        assert_eq!(a, "http://example.test:8001");
        assert_eq!(b, "http://example.test:8002");
        assert_ne!(a, b);
        // Default ports are elided in the canonical
        // serialisation — `https://example.test/foo` and
        // `https://example.test:443/foo` are the same
        // origin per the URL spec.
        assert_eq!(c, "https://example.test");
        assert_eq!(d, "http://example.test");
        assert_ne!(c, d);
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
            .mops_base_url(server.uri())
            .throttle_ms(10)
            .build()
            .await
            .expect_err("503 should fail");
        assert!(matches!(err, BuildError::RobotsStatus { status: 503, .. }));
    }

    #[test]
    fn robots_parser_handles_multi_user_agent_group() {
        // RFC 9309 §2.2.1: a single group may list multiple
        // `User-agent:` lines before its rules. The Disallow
        // applies to ALL of them, so `* + AdsBot` means the
        // star group catches the rule too.
        let body = "\
User-agent: *
User-agent: AdsBot
Disallow: /shared/

User-agent: Googlebot
Disallow: /no-google/
";
        let out = parse_user_agent_star_disallow(body);
        assert_eq!(
            out,
            vec!["/shared/"],
            "shared rule should reach the * group; got {out:?}",
        );
    }

    #[test]
    fn robots_parser_starts_new_group_after_rules() {
        // Once a group emits a rule, the next User-agent
        // line opens a fresh group (RFC 9309 §2.2.1).
        let body = "\
User-agent: *
Disallow: /a/

User-agent: AdsBot
Disallow: /b/
";
        let out = parse_user_agent_star_disallow(body);
        assert_eq!(out, vec!["/a/"]);
    }

    #[test]
    fn robots_parser_is_case_insensitive_on_directive_names() {
        // RFC 9309 §2.2: directive names are
        // case-insensitive (User-Agent vs user-agent vs
        // USER-AGENT all equivalent).
        let body = "\
user-agent: *
DISALLOW: /lower/
";
        let out = parse_user_agent_star_disallow(body);
        assert_eq!(out, vec!["/lower/"]);
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
