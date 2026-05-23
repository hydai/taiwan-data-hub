//! MOEA Business Registry (公司登記 / 商業登記 / 工廠登記)
//! connector (#5b.3).
//!
//! 經濟部商工登記公示資料 is published via the MOEA open-data
//! hub at <https://data.gcis.nat.gov.tw/>. Unlike data.gov.tw
//! there's no single catalog endpoint that lists "all datasets";
//! the well-known feeds live under stable category UUIDs. The
//! catalog walk emits three fixed [`DatasetMetadata`] rows for
//! the registrations Taiwan Data Hub cares about today:
//!
//! - **公司登記資料** (company registry) — `~2M` listed companies.
//!   The flagship dataset; the design doc lists this as the
//!   anchor for the cross-DB "Company 360" playground (#6.4).
//! - **商業登記資料** (business registry) — sole-proprietor &
//!   partnership registrations (i.e. 非公司組織 行號).
//! - **工廠登記資料** (factory registry) — registered
//!   manufacturing sites.
//!
//! All three route to the `economy-business` domain via the
//! substring-match mapper (upstream category `經濟` matches the
//! domain's zh-TW name `經濟與產業`).
//!
//! [`MoeaConnector::list_datasets`] returns those three rows
//! verbatim; the actual per-record HTTP pulls (the eventual
//! `~2M`-row initial sync plus the `更新日期`-keyed incremental
//! described in the issue's Definition of Done) keep the trait
//! `Unsupported` defaults today and land in a follow-up via
//! [`SourceConnector::fetch_data`]. The infrastructure for that
//! follow-up — [`MoeaConnector::polite_get`], throttle, robots
//! cache — lives here so wiring it is a one-liner.
//!
//! Cross-cutting policies match the TWSE connector's shape:
//!
//! - **robots.txt respect** — at construction the builder
//!   fetches `<base>/robots.txt`, parses each `User-agent: *`
//!   group (RFC 9309 §2.2.1: multi-agent groups,
//!   case-insensitive directive names), and stores the
//!   disallow list. Every outbound request consults the list
//!   via [`MoeaConnector::path_allowed`]; a disallowed path
//!   produces [`ConnectorError::Config`] rather than a silent
//!   skip — the worker should DLQ a misconfig loudly. 404
//!   robots.txt is permissive per RFC 9309; any other non-2xx
//!   fails boot.
//! - **per-page throttle** — async-safe minimum interval
//!   between upstream calls, slot-based so concurrent callers
//!   each get a distinct reservation rather than serialising
//!   on the mutex across the sleep.
//!
//! The robots / throttle / path-validator scaffolding here
//! duplicates TWSE's. That is intentional — TWSE just shipped
//! after eight Copilot iteration rounds, the two surfaces will
//! likely diverge as CWA (API-key) and Fishery (different
//! pagination) land, and pulling a shared module today would
//! pick the wrong seams. A `connectors::polite` extraction is
//! the right move once the 4th connector is in flight and we
//! can see what genuinely varies.

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

const DEFAULT_BASE_URL: &str = "https://data.gcis.nat.gov.tw";
const DEFAULT_TIMEOUT_SECS: u64 = 60;
/// Conservative minimum gap between upstream requests. The
/// MOEA open-data hub publishes no rate-limit guidance; 1s
/// keeps us well inside any reasonable interpretation and
/// matches the TWSE connector's default so operators have one
/// number to reason about.
const DEFAULT_THROTTLE_MS: u64 = 1000;
const USER_AGENT: &str = concat!(
    "taiwan-data-hub/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/hydai/taiwan-data-hub)"
);

/// The three MOEA registry feeds the catalog walk emits. The
/// string values become the row's `datasets.source_id` column
/// AND its `slug` (see the per-feed `*_metadata` helpers
/// below), so they need to be stable across releases. The
/// `source_id == slug` equality is intentional today; if the
/// upstream renames its path the fork happens at the constant
/// rather than at the call site.
const DATASET_ID_COMPANY_REGISTRY: &str = "moea-company-registry";
const DATASET_ID_BUSINESS_REGISTRY: &str = "moea-business-registry";
const DATASET_ID_FACTORY_REGISTRY: &str = "moea-factory-registry";

/// HTTP client for the MOEA open-data hub. `Clone` so the
/// worker's per-source cron-job closure can capture an owned
/// copy.
#[derive(Debug, Clone)]
pub struct MoeaConnector {
    http: Client,
    base_url: Url,
    throttle: RequestThrottle,
    /// `robots.txt` disallow paths for the configured base
    /// URL's origin. MOEA today serves a single host, so a
    /// flat `Vec` suffices; if a second origin ever lands
    /// the shape can grow into TWSE's per-origin
    /// [`BTreeMap`] without touching the public API.
    robots_disallowed: Arc<Vec<String>>,
}

impl MoeaConnector {
    /// Construct a connector with production-leaning defaults
    /// (real MOEA host, 1s throttle, robots.txt fetched from
    /// upstream). Use [`Self::builder`] to point at a wiremock
    /// server or tweak the throttle for tests.
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
    /// `/od/data/api/...`). The check is a prefix match
    /// against each disallow entry — matches the
    /// `User-agent: *` directive's semantics for the cases
    /// MOEA publishes.
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

    /// Polite GET against the MOEA host — sleeps on the
    /// throttle, joins the path against the configured base,
    /// refuses disallowed paths (per MOEA's robots.txt), and
    /// issues the request. Wraps the per-request policy the
    /// future `fetch_data` impl will reuse for the per-record
    /// pulls; exposing it now also keeps the stored http /
    /// base-url / throttle fields exercised in the catalog-
    /// only build (no `dead_code` allow needed).
    pub async fn polite_get(&self, path: &str) -> Result<reqwest::Response, crate::ConnectorError> {
        // Reject anything that isn't a same-origin relative
        // path. `Url::join` would otherwise accept an
        // absolute URL (e.g. `https://evil/x`) or a scheme-
        // relative URL (`//evil/x`) and silently swap the
        // origin, bypassing BOTH the intended MOEA
        // restriction and the robots-prefix check. This
        // guard makes the caller's `polite_get(path)`
        // contract honest: the request never leaves the
        // configured host.
        validate_relative_path(path)?;
        let url = self
            .base_url
            .join(path)
            .map_err(|e| crate::ConnectorError::Config(format!("invalid path {path:?}: {e}")))?;
        // Belt + suspenders: even if `validate_relative_path`
        // someday admits a corner case, refuse the request
        // when `Url::join` produced a different origin than
        // the configured base. `Url::origin()` returns Opaque
        // for unusual schemes (file://, data:...), in which
        // case origin equality is `false` for distinct
        // values — which is the safe direction here.
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
/// origin out from under the same-origin contract. The three
/// disallowed shapes are the same set TWSE's guard rejects:
/// absolute URLs (`https://evil/x`), scheme-relative URLs
/// (`//evil/x`), and paths without a leading slash (which
/// resolve against the base URL's path component and become
/// surprising).
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
impl SourceConnector for MoeaConnector {
    fn source_id(&self) -> SourceId {
        SourceId::Moea
    }

    async fn list_datasets(
        &self,
        _cursor: Option<Cursor>,
        _cues: &ConditionalCues,
    ) -> Result<ListResponse, crate::ConnectorError> {
        // MOEA's open-data hub has no single "list all
        // datasets" endpoint — the three known feeds are
        // returned verbatim. ConditionalCues are ignored
        // because there's no upstream ETag / Last-Modified
        // for a synthetic catalog; subsequent runs will emit
        // the same rows and the ETL upsert layer (driver
        // checksum check) will skip-without-rewriting when
        // nothing changed.
        let items = vec![
            company_registry_metadata(),
            business_registry_metadata(),
            factory_registry_metadata(),
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
    // defaults (`ConnectorError::Unsupported`). The eventual
    // ~2M-row initial sync + `更新日期`-keyed incremental
    // described in #5b.3's Definition of Done is a follow-up;
    // the per-record HTTP scaffolding (polite_get + throttle
    // + robots cache) already lives here so the wiring is a
    // one-liner when that work lands.

    fn supports_incremental(&self) -> bool {
        // Flip to `true` once `fetch_data` is implemented.
        false
    }
}

fn company_registry_metadata() -> DatasetMetadata {
    let mut title = BTreeMap::new();
    title.insert("zh-TW".into(), "公司登記資料".into());
    title.insert("en".into(), "MOEA Company Registry".into());
    let mut description = BTreeMap::new();
    description.insert(
        "zh-TW".into(),
        "經濟部商工登記公示資料 — 全國公司登記基本資料(統一編號、名稱、代表人、地址、營業項目)。"
            .into(),
    );
    description.insert(
        "en".into(),
        "Nationwide company registry: uniform business number (統編), \
         legal name, representative, address, business scope."
            .into(),
    );
    DatasetMetadata {
        source_id: DATASET_ID_COMPANY_REGISTRY.into(),
        slug: DATASET_ID_COMPANY_REGISTRY.into(),
        title_i18n: title,
        description_i18n: description,
        license: "OGDL-Taiwan-1.0".into(),
        publisher: Some("經濟部商業發展署".into()),
        update_frequency: Some("daily".into()),
        original_url: Some(
            "https://data.gcis.nat.gov.tw/od/detail?oid=5F64D864-61CB-4D0D-8AD9-492047CC1EA6"
                .into(),
        ),
        last_modified_at: None,
        // The domain mapper's substring match routes "經濟"
        // into the "economy-business" domain (zh_tw name
        // "經濟與產業"). All three MOEA feeds land there.
        upstream_categories: vec!["經濟".into()],
    }
}

fn business_registry_metadata() -> DatasetMetadata {
    let mut title = BTreeMap::new();
    title.insert("zh-TW".into(), "商業登記資料".into());
    title.insert("en".into(), "MOEA Business Registry".into());
    let mut description = BTreeMap::new();
    description.insert(
        "zh-TW".into(),
        "獨資、合夥等非公司組織之商業(行號)登記資料。".into(),
    );
    description.insert(
        "en".into(),
        "Sole-proprietor & partnership business registrations (non-company entities).".into(),
    );
    DatasetMetadata {
        source_id: DATASET_ID_BUSINESS_REGISTRY.into(),
        slug: DATASET_ID_BUSINESS_REGISTRY.into(),
        title_i18n: title,
        description_i18n: description,
        license: "OGDL-Taiwan-1.0".into(),
        publisher: Some("經濟部商業發展署".into()),
        update_frequency: Some("daily".into()),
        original_url: Some(
            "https://data.gcis.nat.gov.tw/od/detail?oid=236EE382-4942-41A9-BD03-CA0709025A7C"
                .into(),
        ),
        last_modified_at: None,
        upstream_categories: vec!["經濟".into()],
    }
}

fn factory_registry_metadata() -> DatasetMetadata {
    let mut title = BTreeMap::new();
    title.insert("zh-TW".into(), "工廠登記資料".into());
    title.insert("en".into(), "MOEA Factory Registry".into());
    let mut description = BTreeMap::new();
    description.insert(
        "zh-TW".into(),
        "全國登記合格工廠之基本資料 — 工廠名稱、地址、產業類別、登記產品。".into(),
    );
    description.insert(
        "en".into(),
        "Registered manufacturing sites: factory name, address, industry category, registered products."
            .into(),
    );
    DatasetMetadata {
        source_id: DATASET_ID_FACTORY_REGISTRY.into(),
        slug: DATASET_ID_FACTORY_REGISTRY.into(),
        title_i18n: title,
        description_i18n: description,
        license: "OGDL-Taiwan-1.0".into(),
        publisher: Some("經濟部產業發展署".into()),
        update_frequency: Some("monthly".into()),
        original_url: Some(
            "https://data.gcis.nat.gov.tw/od/detail?oid=52BA9930-F3FA-4D5E-9E18-A0AC909B05D7"
                .into(),
        ),
        last_modified_at: None,
        upstream_categories: vec!["經濟".into()],
    }
}

/// Async-safe minimum-interval throttle. Tracks the **next
/// allowed instant** rather than the last-tick instant;
/// `tick()` claims the next slot under the lock, drops the
/// guard, then sleeps until its reservation. Concurrent
/// callers each get a distinct slot rather than serialising
/// on the mutex across the sleep.
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

    /// Reserve the next slot and sleep until it. The lock is
    /// held only long enough to mutate the timestamp — the
    /// `sleep_until` happens **after** the guard drops so
    /// other callers can claim their own slots concurrently.
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
        // Sleeping with the guard dropped is what lets
        // concurrent callers each get their own slot —
        // they queue under the lock for ~µs then sleep in
        // parallel until their respective reservations.
        tokio::time::sleep_until(deadline).await;
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// `which` names the configuration setting that held
    /// the bad value (e.g. `base_url`) so operators can
    /// locate it without reading the parser detail in
    /// context. `value` carries the offending string
    /// verbatim — a stale env var or typo is immediately
    /// visible. The underlying `url::ParseError` is
    /// preserved via `#[source]` for chain walkers.
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

/// Builder for [`MoeaConnector`]. The auto-fetch-robots
/// behaviour is opt-out: production wants robots.txt
/// honoured, but tests pointing at wiremock don't want the
/// connector to try the real MOEA host. Tests can either
/// stub a robots.txt route on wiremock OR pass
/// `auto_fetch_robots(false)` to skip the fetch entirely.
#[derive(Debug, Clone)]
pub struct Builder {
    base_url: String,
    timeout_secs: u64,
    throttle_ms: u64,
    /// When `false`, skip the robots.txt fetch at build
    /// time and treat ALL paths as allowed. Test-only.
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

    pub async fn build(self) -> Result<MoeaConnector, BuildError> {
        let base_url = Url::parse(&self.base_url).map_err(|e| BuildError::InvalidUrl {
            which: "base_url",
            value: self.base_url.clone(),
            source: e,
        })?;
        // Disable HTTP redirects so the same-origin and
        // robots-prefix checks above stay authoritative. A
        // 3xx `Location` could point at a different origin,
        // and reqwest's default policy would dutifully
        // follow it — silently bypassing both guards. With
        // `Policy::none()`, redirects surface as ordinary
        // non-2xx responses and `polite_get` returns them
        // through the normal `BadStatus` path.
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
        Ok(MoeaConnector {
            http,
            base_url,
            throttle,
            robots_disallowed: Arc::new(robots_disallowed),
        })
    }
}

/// Fetch `<base>/robots.txt`, parse, and return the disallow
/// paths under `User-agent: *`. Errors are bubbled rather
/// than swallowed — a connector that can't read robots.txt
/// at boot shouldn't run crawls. Goes through the throttle
/// so even the bootstrap fetch is polite.
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
/// `scheme://host[:port]` with default ports elided. MOEA
/// today serves a single origin so this helper is only used
/// by error messages and the (future) per-origin map if a
/// second subdomain ever lands.
fn origin_key(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// Pull `Disallow: ...` lines that fall under any group
/// whose `User-agent:` membership includes `*`. RFC 9309
/// §2.2.1 says a single group may list multiple
/// `User-agent:` lines before its rules; the Disallow
/// applies to ALL of them, so `* + AdsBot` counts as a star
/// group and we collect its rules. Directive names are
/// matched case-insensitively per §2.2.
///
/// Group termination follows §2.2: groups are separated by
/// blank lines. A blank line resets the current-agent set
/// AND the collecting-rules flag so a subsequent
/// `User-agent:` starts fresh — without this, an empty
/// `User-agent: *` group followed by a blank line + another
/// group would leak `*` into the second group's membership
/// (caught by Copilot in #5b.3 review). The TWSE
/// connector's identical-shaped parser still has the same
/// bug; fix lands separately so this PR's scope stays on
/// the MOEA connector itself.
fn parse_user_agent_star_disallow(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current_agents: Vec<String> = Vec::new();
    // `collecting_rules` flips to `true` on the first
    // Allow/Disallow inside a group. A subsequent
    // `User-agent:` then starts a NEW group (so we clear
    // `current_agents`) — that's what distinguishes
    // "another agent joining this group" from "a new group
    // begins".
    let mut collecting_rules = false;
    for raw_line in body.lines() {
        // Strip inline comments (`#` to end of line) before
        // any other parsing so `Disallow: /x  # private`
        // produces `/x`.
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            // RFC 9309 §2.2: groups are separated by blank
            // lines. End the current group so the next
            // `User-agent:` (if any) starts fresh.
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

    #[test]
    fn source_id_is_moea() {
        // The trait method must return SourceId::Moea so the
        // ETL writer routes the rows to the right
        // `datasets.source` SQL token.
        let connector = MoeaConnector {
            http: Client::new(),
            base_url: Url::parse(DEFAULT_BASE_URL).unwrap(),
            throttle: RequestThrottle::new(Duration::from_millis(1)),
            robots_disallowed: Arc::new(Vec::new()),
        };
        assert_eq!(connector.source_id(), SourceId::Moea);
    }

    #[test]
    fn company_registry_metadata_routes_to_economy_business_domain() {
        let d = company_registry_metadata();
        assert_eq!(d.source_id, DATASET_ID_COMPANY_REGISTRY);
        assert_eq!(d.slug, DATASET_ID_COMPANY_REGISTRY);
        assert_eq!(d.upstream_categories, vec!["經濟"]);
        assert!(d.title_i18n.contains_key("zh-TW"));
        assert!(d.title_i18n.contains_key("en"));
        assert!(d.description_i18n.contains_key("zh-TW"));
    }

    #[test]
    fn business_registry_metadata_routes_to_economy_business_domain() {
        let d = business_registry_metadata();
        assert_eq!(d.source_id, DATASET_ID_BUSINESS_REGISTRY);
        assert_eq!(d.slug, DATASET_ID_BUSINESS_REGISTRY);
        assert_eq!(d.upstream_categories, vec!["經濟"]);
    }

    #[test]
    fn factory_registry_metadata_routes_to_economy_business_domain() {
        let d = factory_registry_metadata();
        assert_eq!(d.source_id, DATASET_ID_FACTORY_REGISTRY);
        assert_eq!(d.slug, DATASET_ID_FACTORY_REGISTRY);
        assert_eq!(d.upstream_categories, vec!["經濟"]);
    }

    #[tokio::test]
    async fn list_datasets_returns_three_fixed_rows() {
        // The catalog walk emits exactly the three registry
        // feeds; total is set so downstream paginators can
        // log "1/N done" without an extra fetch.
        let connector = MoeaConnector::builder()
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
        assert_eq!(page.items.len(), 3);
        assert_eq!(page.total, Some(3));
        assert!(page.next.is_none());
        let source_ids: Vec<_> = page.items.iter().map(|d| d.source_id.as_str()).collect();
        assert_eq!(
            source_ids,
            vec![
                DATASET_ID_COMPANY_REGISTRY,
                DATASET_ID_BUSINESS_REGISTRY,
                DATASET_ID_FACTORY_REGISTRY,
            ]
        );
    }

    #[tokio::test]
    async fn supports_incremental_is_false_today() {
        // The flag gates the driver's per-record path; until
        // `fetch_data` lands it MUST be false so the driver
        // stays on the catalog-only branch.
        let connector = MoeaConnector::builder()
            .base_url("https://example.test")
            .auto_fetch_robots(false)
            .build()
            .await
            .unwrap();
        assert!(!connector.supports_incremental());
    }

    #[test]
    fn validate_relative_path_accepts_canonical_paths() {
        assert!(validate_relative_path("/od/data/api/COMPANY").is_ok());
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
        // A path without a leading slash would be resolved
        // against the base URL's path component and become
        // surprising. Force callers to be explicit.
        let err = validate_relative_path("od/data/api/COMPANY").unwrap_err();
        assert!(
            matches!(&err, crate::ConnectorError::Config(msg) if msg.contains("start with '/'")),
            "got {err:?}",
        );
        assert!(validate_relative_path("").is_err());
    }

    #[tokio::test]
    async fn polite_get_rejects_absolute_url_path() {
        // End-to-end check on the polite_get path: even if
        // `Url::join` parses the absolute URL, the pre-join
        // validator catches it. Uses a wiremock server that
        // would respond 200 if the request somehow reached
        // it; the test asserts we never get that far.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let connector = MoeaConnector::builder()
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
        // Defence-in-depth: even though `polite_get`
        // validates the request URL against the configured
        // origin, the HTTP client's redirect policy could
        // quietly carry the request to a different host via
        // `Location`. We disable redirects at the builder so
        // a 3xx surfaces as a `BadStatus` instead of being
        // followed — keeping the same-origin and
        // robots-prefix checks authoritative.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/redirect-me"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("Location", "https://evil.example/owned"),
            )
            .mount(&server)
            .await;
        let connector = MoeaConnector::builder()
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
        // Wiremock stubs robots.txt with a `Disallow: /api`
        // entry. A subsequent polite_get against `/api/...`
        // must be rejected before any HTTP call goes out.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/robots.txt"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("User-agent: *\nDisallow: /api\n"),
            )
            .mount(&server)
            .await;
        let connector = MoeaConnector::builder()
            .base_url(server.uri())
            .throttle_ms(10)
            .build()
            .await
            .unwrap();
        let err = connector
            .polite_get("/api/secret")
            .await
            .expect_err("disallowed path must be rejected");
        assert!(
            matches!(&err, crate::ConnectorError::Config(msg) if msg.contains("disallowed by robots.txt")),
            "got {err:?}",
        );
    }

    #[test]
    fn origin_key_includes_scheme_and_port() {
        // Origin key per RFC 9309 §2.1 must include all
        // three components so a host serving different
        // rules on different ports / schemes can't collide.
        let a = origin_key(&Url::parse("http://example.test:8001/foo").unwrap());
        let b = origin_key(&Url::parse("http://example.test:8002/foo").unwrap());
        let c = origin_key(&Url::parse("https://example.test/foo").unwrap());
        let d = origin_key(&Url::parse("http://example.test/foo").unwrap());
        assert_eq!(a, "http://example.test:8001");
        assert_eq!(b, "http://example.test:8002");
        assert_ne!(a, b);
        assert_eq!(c, "https://example.test");
        assert_eq!(d, "http://example.test");
        assert_ne!(c, d);
    }

    #[tokio::test]
    async fn build_treats_robots_404_as_permissive() {
        // RFC 9309 §2.3.1.3: a 404 robots.txt means "no
        // restrictions". The builder MUST treat this as a
        // success and return an empty disallow list — NOT
        // bail.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/robots.txt"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let connector = MoeaConnector::builder()
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
        let err = MoeaConnector::builder()
            .base_url(server.uri())
            .throttle_ms(10)
            .build()
            .await
            .expect_err("503 should fail");
        assert!(matches!(err, BuildError::RobotsStatus { status: 503, .. }));
    }

    #[tokio::test]
    async fn build_error_invalid_url_carries_input_value() {
        // The struct variant names the setting AND echoes
        // the value back so operators can find the
        // misconfiguration without re-deriving it from a
        // parser-level message.
        let err = MoeaConnector::builder()
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
        let rendered = err.to_string();
        assert!(rendered.contains("base_url"), "got {rendered:?}");
        assert!(rendered.contains("not a url"), "got {rendered:?}");
    }

    #[test]
    fn robots_parser_extracts_star_disallow() {
        let body = "User-agent: *\nDisallow: /admin\nDisallow: /private\n";
        let out = parse_user_agent_star_disallow(body);
        assert_eq!(out, vec!["/admin".to_string(), "/private".to_string()]);
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
        // RFC 9309 §2.2: groups are separated by blank
        // lines. The first `User-agent: *` has no rules,
        // the blank line ends that empty group, and the
        // subsequent `User-agent: GoogleBot` must start
        // fresh — `*` MUST NOT leak into the GoogleBot
        // group's membership, so `/private` is NOT a
        // star-group rule and must not be collected.
        let body = "\
User-agent: *\n\
\n\
User-agent: GoogleBot\n\
Disallow: /private\n";
        let out = parse_user_agent_star_disallow(body);
        assert!(out.is_empty(), "got {out:?}");
    }

    #[test]
    fn robots_parser_blank_line_after_rules_terminates_group() {
        // Same group-termination semantics when the `*`
        // group has rules: the blank line ends the group
        // so a subsequent GoogleBot group's rules are not
        // collected.
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
    async fn throttle_does_not_block_first_call() {
        // The first tick starts at the current virtual
        // instant — no prior reservation, no wait.
        let throttle = RequestThrottle::new(Duration::from_millis(50));
        let start = Instant::now();
        throttle.tick().await;
        let elapsed = Instant::now() - start;
        assert_eq!(elapsed, Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn throttle_enforces_minimum_interval_between_ticks() {
        // Second sequential tick must wait one full
        // `min_interval` because the first reserved
        // `now + min_interval` as the next slot.
        let throttle = RequestThrottle::new(Duration::from_millis(50));
        throttle.tick().await;
        let start = Instant::now();
        throttle.tick().await;
        let elapsed = Instant::now() - start;
        assert_eq!(elapsed, Duration::from_millis(50));
    }

    #[tokio::test(start_paused = true)]
    async fn throttle_concurrent_callers_get_distinct_slots() {
        // Three callers tick concurrently; the third must
        // wait two full intervals (slots 0, 1, 2 → callers
        // get 0, 50ms, 100ms).
        let throttle = RequestThrottle::new(Duration::from_millis(50));
        let start = Instant::now();
        let t1 = tokio::spawn({
            let t = throttle.clone();
            async move { t.tick().await }
        });
        let t2 = tokio::spawn({
            let t = throttle.clone();
            async move { t.tick().await }
        });
        let t3 = tokio::spawn({
            let t = throttle.clone();
            async move { t.tick().await }
        });
        // Join in order; the LAST one to complete reflects
        // the longest wait.
        t1.await.unwrap();
        t2.await.unwrap();
        t3.await.unwrap();
        let elapsed = Instant::now() - start;
        assert_eq!(elapsed, Duration::from_millis(100));
    }
}
