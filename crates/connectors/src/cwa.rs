//! CWA (中央氣象署) connector (#5b.4).
//!
//! Central Weather Administration of Taiwan publishes the
//! national observation network, forecasts, and severe-weather
//! tracks via <https://opendata.cwa.gov.tw>. Unlike TWSE and
//! MOEA, **CWA requires an API key**: every request must carry
//! `?Authorization=<CWA_API_KEY>` as a query parameter. Without
//! a key the upstream returns HTTP 401.
//!
//! The catalog walk emits three fixed [`DatasetMetadata`] rows
//! covering the feeds Taiwan Data Hub cares about today:
//!
//! - **自動氣象站 — 氣象觀測資料** (real-time observations,
//!   dataset `O-A0001-001`) — temperature, humidity, wind, etc.
//!   from CWA's automated stations across the country.
//! - **一般天氣預報 — 今明 36 小時天氣預報** (dataset
//!   `F-C0032-001`) — county-level 36-hour outlook.
//! - **颱風路徑** (dataset `W-C0034-005`) — active and recent
//!   typhoon track polylines.
//!
//! All three carry `upstream_categories = ["環境"]` so the
//! domain mapper's substring match routes them into the
//! `environment` domain. CWA's native categorisation uses 氣象
//! / 天氣 tags that don't substring-match any of the 20 shipped
//! domains; emitting `"環境"` is the pragmatic close-fit that
//! keeps the rows discoverable today. A future taxonomy revision
//! that adds a dedicated weather bucket can flip this without
//! touching the connector.
//!
//! [`CwaConnector::list_datasets`] returns those three rows
//! verbatim. The per-dataset HTTP pulls land in a follow-up via
//! [`SourceConnector::fetch_data`]; the polite-GET scaffolding
//! ([`CwaConnector::polite_get`] — same shape as MOEA, but
//! with the key auto-injected) is already here so the wiring
//! will be a one-liner.
//!
//! ## API key handling
//!
//! Production reads the key from the `CWA_API_KEY` environment
//! variable. [`CwaConnector::new`] is a thin `Builder::default
//! → build` wrapper that performs that env read; tests use
//! [`Builder::api_key`] to inject a fixture key without
//! touching `std::env`. When [`Builder::api_key`] is unset AND
//! the env var is missing/empty, [`Builder::build`] returns
//! [`BuildError::MissingApiKey`] so an `enabled = true` row in
//! `sources.toml` without a key fails boot loudly — better
//! than a silently-401-ing crawl every 6 hours.
//!
//! Sign-up flow: [`docs/sources/cwa.md`](../../../docs/sources/cwa.md).
//!
//! ## Cross-cutting policies
//!
//! Same shape as the TWSE / MOEA connectors:
//!
//! - **robots.txt respect** — Builder fetches `<base>/robots.txt`
//!   at construction (RFC 9309: §2.1 origin scoping, §2.2
//!   blank-line group termination, §2.2.1 multi-agent groups,
//!   §2.2 case-insensitive directive names). Disallowed paths
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
//! duplicates MOEA's. Intentional — see the matching note in
//! `crates/connectors/src/moea.rs`: a `connectors::polite`
//! extraction is the right move once we can see what genuinely
//! varies across all four connectors.

use std::collections::BTreeMap;
use std::env;
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

const DEFAULT_BASE_URL: &str = "https://opendata.cwa.gov.tw";
const DEFAULT_TIMEOUT_SECS: u64 = 60;
/// Conservative minimum gap between upstream requests. CWA's
/// rate-limit guidance is informal; 1s keeps us well inside
/// any reasonable interpretation and matches the TWSE / MOEA
/// connectors' default so operators have one number to reason
/// about.
const DEFAULT_THROTTLE_MS: u64 = 1000;
/// Environment variable the production builder reads at boot.
/// Surfaced as a constant so tests + docs reference one name.
const API_KEY_ENV: &str = "CWA_API_KEY";
/// Query-parameter name CWA expects the key under. Spelled
/// exactly `Authorization` (capital A) per the open-data hub's
/// API — case differs from HTTP's standard `Authorization`
/// header, and the upstream rejects the lowercase form.
const API_KEY_QUERY_PARAM: &str = "Authorization";
const USER_AGENT: &str = concat!(
    "taiwan-data-hub/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/hydai/taiwan-data-hub)"
);

/// The three CWA feeds the catalog walk emits. The string
/// values become the row's `datasets.source_id` AND its
/// `slug` (see the per-feed `*_metadata` helpers below), so
/// they need to be stable across releases. The
/// `source_id == slug` equality is intentional today; if the
/// upstream renames its dataset id the fork happens at the
/// constant rather than at the call site.
const DATASET_ID_OBSERVATIONS: &str = "cwa-observations";
const DATASET_ID_TOWNSHIP_FORECAST: &str = "cwa-township-forecast";
const DATASET_ID_TYPHOON_TRACK: &str = "cwa-typhoon-track";

/// HTTP client for the CWA open-data hub. `Clone` so the
/// worker's per-source cron-job closure can capture an owned
/// copy. The API key is held here (not on the `Client`) so
/// `polite_get` can inject it as the query parameter CWA
/// requires.
#[derive(Debug, Clone)]
pub struct CwaConnector {
    http: Client,
    base_url: Url,
    api_key: ApiKey,
    throttle: RequestThrottle,
    /// `robots.txt` disallow paths for the configured base
    /// URL's origin. CWA today serves a single host, so a
    /// flat `Vec` suffices.
    robots_disallowed: Arc<Vec<String>>,
}

/// Newtype wrapper so the key can't accidentally land in a
/// generic `Debug` log line. The `Debug` impl is custom and
/// always renders `"<redacted>"`; the cleartext is only ever
/// exposed via [`Self::expose`], which is the one place a
/// reviewer should look when checking key handling.
#[derive(Clone)]
struct ApiKey(String);

impl ApiKey {
    fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

impl CwaConnector {
    /// Construct a connector with production defaults — real
    /// CWA host, 1s throttle, robots.txt fetched from
    /// upstream, API key read from `CWA_API_KEY`. Use
    /// [`Self::builder`] to point at a wiremock server or
    /// inject a fixture key.
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
    /// list? `path` is the URL path component (e.g.
    /// `/api/v1/rest/datastore/O-A0001-001`).
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

    /// Polite GET against the CWA host — sleeps on the
    /// throttle, joins the path against the configured base,
    /// refuses disallowed paths, AND auto-appends the
    /// `Authorization` query parameter CWA requires. Wraps
    /// the per-request policy the future `fetch_data` impl
    /// will reuse for the per-dataset pulls; exposing it now
    /// also keeps the stored http / base-url / throttle /
    /// `api_key` fields exercised in the catalog-only build
    /// (no `dead_code` allow needed).
    pub async fn polite_get(&self, path: &str) -> Result<reqwest::Response, crate::ConnectorError> {
        // Reject anything that isn't a same-origin relative
        // path. `Url::join` would otherwise accept an
        // absolute URL (e.g. `https://evil/x`) or a scheme-
        // relative URL (`//evil/x`) and silently swap the
        // origin, bypassing BOTH the intended CWA
        // restriction and the robots-prefix check. This
        // guard makes the caller's `polite_get(path)`
        // contract honest: the request never leaves the
        // configured host.
        validate_relative_path(path)?;
        let mut url = self.base_url.join(path).map_err(|e| {
            crate::ConnectorError::Config(format!("invalid path {:?}: {e}", redact_query(path)))
        })?;
        // Belt + suspenders: even if `validate_relative_path`
        // someday admits a corner case, refuse the request
        // when `Url::join` produced a different origin than
        // the configured base.
        if url.origin() != self.base_url.origin() {
            return Err(crate::ConnectorError::Config(format!(
                "path {:?} resolved to a different origin than {}",
                redact_query(path),
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
        // Refuse to inject if the caller's path already
        // carries an `Authorization` query parameter — two
        // values would land on the wire and upstream's
        // first-wins-vs-last-wins behaviour decides which
        // key gets used. Silently stripping would mask a
        // real misconfiguration (e.g. a stale key copied
        // from somewhere); rejecting forces the caller to
        // look at their `path`. Case-insensitive match in
        // case a caller spells it `authorization` even
        // though CWA itself requires the capital-A form.
        if url
            .query_pairs()
            .any(|(k, _)| k.eq_ignore_ascii_case(API_KEY_QUERY_PARAM))
        {
            // CRITICAL: do NOT echo the full path here. The
            // whole point of this guard is that the caller's
            // query string may carry a real (stale) key under
            // `Authorization=...`; logging it verbatim would
            // defeat the `ApiKey` newtype's redaction.
            return Err(crate::ConnectorError::Config(format!(
                "path {:?} already specifies an {API_KEY_QUERY_PARAM:?} query \
                 parameter — the connector injects the API key automatically and \
                 refuses to send a duplicate",
                redact_query(path),
            )));
        }
        // Inject the API key as a query parameter. Done after
        // the origin + robots checks so a typo in `path` can't
        // accidentally smuggle the key to a different host.
        url.query_pairs_mut()
            .append_pair(API_KEY_QUERY_PARAM, self.api_key.expose());
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
            "path {:?} must start with '/' (got a non-relative path)",
            redact_query(path),
        )));
    }
    if path.starts_with("//") {
        return Err(crate::ConnectorError::Config(format!(
            "path {:?} must not start with '//' (scheme-relative URLs are forbidden)",
            redact_query(path),
        )));
    }
    if path.contains("://") {
        return Err(crate::ConnectorError::Config(format!(
            "path {:?} must not contain '://' (absolute URLs are forbidden)",
            redact_query(path),
        )));
    }
    Ok(())
}

/// Strip the query string before echoing a path back into
/// an error message. Callers may inadvertently include a
/// real `Authorization=...` value in the query string (the
/// exact misconfiguration the duplicate-rejection guard
/// catches); echoing the full path verbatim would defeat
/// the `ApiKey` newtype's redaction and leak the key into
/// logs. The path component alone is sufficient context for
/// debugging the shape error.
fn redact_query(path: &str) -> &str {
    path.split_once('?').map_or(path, |(prefix, _)| prefix)
}

#[async_trait]
impl SourceConnector for CwaConnector {
    fn source_id(&self) -> SourceId {
        SourceId::Cwa
    }

    async fn list_datasets(
        &self,
        _cursor: Option<Cursor>,
        _cues: &ConditionalCues,
    ) -> Result<ListResponse, crate::ConnectorError> {
        // CWA's catalog isn't enumerable via a single
        // endpoint — the three known feeds are returned
        // verbatim. ConditionalCues are ignored because
        // there's no upstream ETag for a synthetic catalog;
        // subsequent runs emit the same rows and the ETL
        // upsert layer (driver checksum check) will skip-
        // without-rewriting when nothing changed.
        let items = vec![
            observations_metadata(),
            township_forecast_metadata(),
            typhoon_track_metadata(),
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

    fn supports_incremental(&self) -> bool {
        // Flip to `true` once `fetch_data` is implemented
        // (the per-dataset pulls that read the actual
        // observation / forecast / typhoon-track JSON).
        false
    }
}

fn observations_metadata() -> DatasetMetadata {
    let mut title = BTreeMap::new();
    title.insert("zh-TW".into(), "自動氣象站 — 氣象觀測資料".into());
    title.insert(
        "en".into(),
        "CWA Automated Weather Station Observations".into(),
    );
    let mut description = BTreeMap::new();
    description.insert(
        "zh-TW".into(),
        "全國自動氣象站即時觀測:氣溫、相對濕度、風向、風速、雨量、氣壓等。".into(),
    );
    description.insert(
        "en".into(),
        "Real-time observations from CWA's nationwide automated weather stations: \
         temperature, humidity, wind direction and speed, rainfall, pressure."
            .into(),
    );
    DatasetMetadata {
        source_id: DATASET_ID_OBSERVATIONS.into(),
        slug: DATASET_ID_OBSERVATIONS.into(),
        title_i18n: title,
        description_i18n: description,
        license: "OGDL-Taiwan-1.0".into(),
        publisher: Some("中央氣象署".into()),
        update_frequency: Some("hourly".into()),
        original_url: Some("https://opendata.cwa.gov.tw/dataset/observation/O-A0001-001".into()),
        last_modified_at: None,
        // CWA's native categories (氣象 / 天氣) don't
        // substring-match any of the 20 shipped domain names;
        // "環境" is the pragmatic close-fit (matches the
        // `environment` domain exactly). A future taxonomy
        // revision can flip this if a dedicated weather
        // bucket lands.
        upstream_categories: vec!["環境".into()],
    }
}

fn township_forecast_metadata() -> DatasetMetadata {
    let mut title = BTreeMap::new();
    title.insert("zh-TW".into(), "一般天氣預報 — 今明 36 小時天氣預報".into());
    title.insert("en".into(), "CWA 36-hour General Weather Forecast".into());
    let mut description = BTreeMap::new();
    description.insert(
        "zh-TW".into(),
        "全國 22 縣市未來 36 小時天氣概況、最高/最低溫度、降雨機率、舒適度。".into(),
    );
    description.insert(
        "en".into(),
        "36-hour outlook for all 22 counties: condition summary, high/low \
         temperatures, precipitation probability, comfort index."
            .into(),
    );
    DatasetMetadata {
        source_id: DATASET_ID_TOWNSHIP_FORECAST.into(),
        slug: DATASET_ID_TOWNSHIP_FORECAST.into(),
        title_i18n: title,
        description_i18n: description,
        license: "OGDL-Taiwan-1.0".into(),
        publisher: Some("中央氣象署".into()),
        update_frequency: Some("6h".into()),
        original_url: Some("https://opendata.cwa.gov.tw/dataset/forecast/F-C0032-001".into()),
        last_modified_at: None,
        upstream_categories: vec!["環境".into()],
    }
}

fn typhoon_track_metadata() -> DatasetMetadata {
    let mut title = BTreeMap::new();
    title.insert("zh-TW".into(), "颱風路徑".into());
    title.insert("en".into(), "CWA Typhoon Tracks".into());
    let mut description = BTreeMap::new();
    description.insert(
        "zh-TW".into(),
        "現行與近期颱風的路徑資料,含時間、座標、強度與七級暴風圈半徑。".into(),
    );
    description.insert(
        "en".into(),
        "Active and recent typhoon tracks: timestamps, coordinates, intensity, \
         radius of gale-force winds."
            .into(),
    );
    DatasetMetadata {
        source_id: DATASET_ID_TYPHOON_TRACK.into(),
        slug: DATASET_ID_TYPHOON_TRACK.into(),
        title_i18n: title,
        description_i18n: description,
        license: "OGDL-Taiwan-1.0".into(),
        publisher: Some("中央氣象署".into()),
        update_frequency: Some("as published".into()),
        original_url: Some("https://opendata.cwa.gov.tw/dataset/warning/W-C0034-005".into()),
        last_modified_at: None,
        upstream_categories: vec!["環境".into()],
    }
}

/// Async-safe minimum-interval throttle. Same shape as
/// MOEA's — slot-based so concurrent callers each get a
/// distinct reservation rather than serialising on the
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
    /// CWA requires an API key — without one every request
    /// would 401. Fail boot loudly so an operator who
    /// forgot to set `CWA_API_KEY` doesn't see silent
    /// failures every 6 hours when the cron fires.
    #[error(
        "CWA API key missing — set {env_var} in the environment (signup: \
         docs/sources/cwa.md) or pass Builder::api_key for tests"
    )]
    MissingApiKey { env_var: &'static str },
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

/// Builder for [`CwaConnector`]. Three knobs differ from
/// the MOEA/TWSE builders:
///
/// 1. [`Self::api_key`] — explicit key. Takes precedence
///    over the env var. Test-only in practice; production
///    leaves this unset and lets [`Self::build`] read
///    `CWA_API_KEY`.
/// 2. [`Self::auto_fetch_robots`] — same opt-out as the
///    other builders; tests pointing at wiremock don't
///    want the connector to try the real CWA host.
/// 3. (Implicit) the build step will fail with
///    [`BuildError::MissingApiKey`] when neither the
///    explicit key nor the env var is set — a flipped
///    `enabled = true` row in `sources.toml` without a
///    key fails boot rather than silently 401-ing.
#[derive(Debug, Clone)]
pub struct Builder {
    base_url: String,
    api_key: Option<String>,
    timeout_secs: u64,
    throttle_ms: u64,
    auto_fetch_robots: bool,
    /// When `true`, [`Self::build`] skips the `CWA_API_KEY`
    /// env lookup entirely and falls straight through to
    /// the missing-key error if no explicit key was set.
    /// Test-only: keeps the missing-key assertion
    /// deterministic on dev machines that happen to have
    /// `CWA_API_KEY` exported.
    #[cfg(test)]
    skip_env_lookup: bool,
}

impl Default for Builder {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            api_key: None,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            throttle_ms: DEFAULT_THROTTLE_MS,
            auto_fetch_robots: true,
            #[cfg(test)]
            skip_env_lookup: false,
        }
    }
}

impl Builder {
    #[must_use]
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Set the API key explicitly. Takes precedence over the
    /// `CWA_API_KEY` env var. Use this from tests so the
    /// production env var doesn't leak into the test run; in
    /// production, leave it unset and let [`Self::build`]
    /// read the env.
    #[must_use]
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
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

    /// Test-only: bypass the `CWA_API_KEY` env lookup.
    /// Lets `build_fails_loudly_when_api_key_missing` assert
    /// the error path deterministically even when a dev
    /// machine has `CWA_API_KEY` exported.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn skip_env_lookup(mut self, on: bool) -> Self {
        self.skip_env_lookup = on;
        self
    }

    pub async fn build(self) -> Result<CwaConnector, BuildError> {
        let base_url = Url::parse(&self.base_url).map_err(|e| BuildError::InvalidUrl {
            which: "base_url",
            value: self.base_url.clone(),
            source: e,
        })?;
        // Explicit key wins; otherwise read CWA_API_KEY. An
        // env var set to the empty string counts as missing —
        // a real key is never empty, and silently sending
        // `?Authorization=` would still produce 401 with a
        // confusingly-different error path.
        #[cfg(test)]
        let consult_env = !self.skip_env_lookup;
        #[cfg(not(test))]
        let consult_env = true;
        let api_key_str = self.api_key.or_else(|| {
            if !consult_env {
                return None;
            }
            env::var(API_KEY_ENV).ok().and_then(|v| {
                let trimmed = v.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_owned())
                }
            })
        });
        let Some(api_key_str) = api_key_str else {
            return Err(BuildError::MissingApiKey {
                env_var: API_KEY_ENV,
            });
        };
        let api_key = ApiKey(api_key_str);
        // Disable HTTP redirects so the same-origin and
        // robots-prefix checks above stay authoritative.
        // Bonus reason here: redirects on a request that
        // carries an API key as a query parameter would
        // leak the key to the redirect target. With
        // `Policy::none()` that can't happen — 3xx surfaces
        // as `BadStatus` before any follow-up is issued.
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
        Ok(CwaConnector {
            http,
            base_url,
            api_key,
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
/// Carries the MOEA-side fix for RFC 9309 §2.2 blank-line
/// group termination: a blank line resets the current-agent
/// set so an empty `*` group can't leak `*` into the next
/// group's membership.
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
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Test helper: a minimal connector skeleton for trait /
    /// metadata assertions that don't need the full builder
    /// (no HTTP, no robots fetch).
    fn skeleton_connector() -> CwaConnector {
        CwaConnector {
            http: Client::new(),
            base_url: Url::parse(DEFAULT_BASE_URL).unwrap(),
            api_key: ApiKey("test-key".into()),
            throttle: RequestThrottle::new(Duration::from_millis(1)),
            robots_disallowed: Arc::new(Vec::new()),
        }
    }

    #[test]
    fn source_id_is_cwa() {
        assert_eq!(skeleton_connector().source_id(), SourceId::Cwa);
    }

    #[test]
    fn observations_metadata_routes_to_environment_domain() {
        let d = observations_metadata();
        assert_eq!(d.source_id, DATASET_ID_OBSERVATIONS);
        assert_eq!(d.slug, DATASET_ID_OBSERVATIONS);
        assert_eq!(d.upstream_categories, vec!["環境"]);
        assert!(d.title_i18n.contains_key("zh-TW"));
        assert!(d.title_i18n.contains_key("en"));
    }

    #[test]
    fn township_forecast_metadata_routes_to_environment_domain() {
        let d = township_forecast_metadata();
        assert_eq!(d.source_id, DATASET_ID_TOWNSHIP_FORECAST);
        assert_eq!(d.slug, DATASET_ID_TOWNSHIP_FORECAST);
        assert_eq!(d.upstream_categories, vec!["環境"]);
    }

    #[test]
    fn typhoon_track_metadata_routes_to_environment_domain() {
        let d = typhoon_track_metadata();
        assert_eq!(d.source_id, DATASET_ID_TYPHOON_TRACK);
        assert_eq!(d.slug, DATASET_ID_TYPHOON_TRACK);
        assert_eq!(d.upstream_categories, vec!["環境"]);
    }

    #[tokio::test]
    async fn list_datasets_returns_three_fixed_rows() {
        let connector = CwaConnector::builder()
            .base_url("https://example.test")
            .api_key("test-key")
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
        let source_ids: Vec<_> = page.items.iter().map(|d| d.source_id.as_str()).collect();
        assert_eq!(
            source_ids,
            vec![
                DATASET_ID_OBSERVATIONS,
                DATASET_ID_TOWNSHIP_FORECAST,
                DATASET_ID_TYPHOON_TRACK,
            ]
        );
    }

    #[tokio::test]
    async fn supports_incremental_is_false_today() {
        let connector = CwaConnector::builder()
            .base_url("https://example.test")
            .api_key("test-key")
            .auto_fetch_robots(false)
            .build()
            .await
            .unwrap();
        assert!(!connector.supports_incremental());
    }

    #[test]
    fn validate_relative_path_accepts_canonical_paths() {
        assert!(validate_relative_path("/api/v1/rest/datastore/O-A0001-001").is_ok());
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
        let err = validate_relative_path("api/v1/rest/datastore/O-A0001-001").unwrap_err();
        assert!(
            matches!(&err, crate::ConnectorError::Config(msg) if msg.contains("start with '/'")),
            "got {err:?}",
        );
        assert!(validate_relative_path("").is_err());
    }

    #[tokio::test]
    async fn polite_get_injects_authorization_query_param() {
        // The whole point of holding the API key in the
        // connector: every outbound request MUST carry
        // `?Authorization=<key>`. Wiremock will only match
        // when that exact param is present, so an absent
        // / misspelled param shows up as a 404 from the
        // mock — which the test asserts is NOT what we see.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/rest/datastore/O-A0001-001"))
            .and(query_param("Authorization", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let connector = CwaConnector::builder()
            .base_url(server.uri())
            .api_key("test-key")
            .throttle_ms(10)
            .auto_fetch_robots(false)
            .build()
            .await
            .unwrap();
        let resp = connector
            .polite_get("/api/v1/rest/datastore/O-A0001-001")
            .await
            .expect("authorized request succeeds");
        assert_eq!(resp.status().as_u16(), 200);
    }

    #[tokio::test]
    async fn polite_get_rejects_absolute_url_path() {
        // Even though `Url::join` parses the absolute URL,
        // the pre-join validator catches it BEFORE the API
        // key gets injected — so a typo'd path can't leak
        // the key to a third-party host.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let connector = CwaConnector::builder()
            .base_url(server.uri())
            .api_key("test-key")
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
    async fn polite_get_rejects_path_with_duplicate_authorization_param() {
        // If a caller's path already carries `Authorization`,
        // appending the connector's key would put two values
        // on the wire — upstream's first-wins-vs-last-wins
        // behaviour decides which key gets used. We reject
        // loudly rather than silently strip so a real
        // misconfiguration surfaces immediately.
        let connector = CwaConnector::builder()
            .base_url("https://example.test")
            .api_key("test-key")
            .auto_fetch_robots(false)
            .build()
            .await
            .unwrap();
        let err = connector
            .polite_get("/api/v1/rest/datastore/O-A0001-001?Authorization=stale-key")
            .await
            .expect_err("duplicate Authorization must be rejected");
        assert!(
            matches!(&err, crate::ConnectorError::Config(msg) if msg.contains("Authorization")),
            "got {err:?}",
        );
    }

    #[tokio::test]
    async fn polite_get_rejects_duplicate_authorization_case_insensitively() {
        // CWA itself requires the capital-A form, but if a
        // caller spells it `authorization` lowercase, we
        // still want to refuse — the case mismatch would
        // otherwise let the duplicate land on the wire.
        let connector = CwaConnector::builder()
            .base_url("https://example.test")
            .api_key("test-key")
            .auto_fetch_robots(false)
            .build()
            .await
            .unwrap();
        let err = connector
            .polite_get("/api/v1/rest/datastore/O-A0001-001?authorization=stale-key")
            .await
            .expect_err("case-mismatched duplicate must still be rejected");
        assert!(
            matches!(&err, crate::ConnectorError::Config(msg) if msg.contains("Authorization")),
            "got {err:?}",
        );
    }

    #[tokio::test]
    async fn polite_get_error_does_not_leak_caller_supplied_key() {
        // The duplicate-Authorization guard catches a real
        // misconfiguration: a caller passing `?Authorization=
        // <stale-key>`. The error message MUST NOT echo the
        // key back — that would defeat the `ApiKey` newtype's
        // redaction by leaking the stale value into logs.
        let connector = CwaConnector::builder()
            .base_url("https://example.test")
            .api_key("test-key")
            .auto_fetch_robots(false)
            .build()
            .await
            .unwrap();
        let leak_marker = "MUST-NOT-APPEAR-IN-ERROR-XYZ123";
        let err = connector
            .polite_get(&format!(
                "/api/v1/rest/datastore/O-A0001-001?Authorization={leak_marker}",
            ))
            .await
            .expect_err("duplicate must be rejected");
        let rendered = err.to_string();
        assert!(
            !rendered.contains(leak_marker),
            "error message leaked caller's key: {rendered:?}"
        );
        // The path component (sans query) IS allowed — and
        // useful — for debugging the shape of the problem.
        assert!(
            rendered.contains("/api/v1/rest/datastore/O-A0001-001"),
            "error should still name the path component: {rendered:?}"
        );
    }

    #[test]
    fn redact_query_strips_query_string() {
        assert_eq!(redact_query("/api/x?Authorization=secret"), "/api/x");
        assert_eq!(redact_query("/api/x?a=1&b=2"), "/api/x");
        // No query string: pass through unchanged.
        assert_eq!(redact_query("/api/x"), "/api/x");
        // Empty path: pass through.
        assert_eq!(redact_query(""), "");
        // Fragment without query: pass through (fragment is
        // an upstream-side detail, never carries a key).
        assert_eq!(redact_query("/api/x#frag"), "/api/x#frag");
    }

    #[tokio::test]
    async fn polite_get_does_not_follow_redirects() {
        // Defence-in-depth: a 3xx Location header could
        // bounce us to a different host AND the redirect
        // request would carry the API key in the query
        // string — leaking it. Redirects disabled at the
        // builder so 3xx surfaces as BadStatus and the
        // follow-up is never issued.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/redirect-me"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("Location", "https://evil.example/owned"),
            )
            .mount(&server)
            .await;
        let connector = CwaConnector::builder()
            .base_url(server.uri())
            .api_key("test-key")
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
                ResponseTemplate::new(200).set_body_string("User-agent: *\nDisallow: /api\n"),
            )
            .mount(&server)
            .await;
        let connector = CwaConnector::builder()
            .base_url(server.uri())
            .api_key("test-key")
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

    #[tokio::test]
    async fn build_fails_loudly_when_api_key_missing() {
        // Deterministic regardless of the dev machine's env:
        // `skip_env_lookup(true)` makes `build()` ignore
        // `CWA_API_KEY`. The unsafe `env::remove_var` route
        // would have worked too, but the workspace forbids
        // `unsafe` so we route around it.
        let err = CwaConnector::builder()
            .base_url("https://example.test")
            .auto_fetch_robots(false)
            .skip_env_lookup(true)
            .build()
            .await
            .expect_err("missing key must fail boot");
        match &err {
            BuildError::MissingApiKey { env_var } => {
                assert_eq!(*env_var, API_KEY_ENV);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
        // Display includes both the env var name and the
        // signup doc path so a confused operator has the
        // breadcrumb in front of them.
        let rendered = err.to_string();
        assert!(rendered.contains(API_KEY_ENV), "got {rendered:?}");
        assert!(rendered.contains("docs/sources/cwa.md"), "got {rendered:?}");
    }

    #[tokio::test]
    async fn build_uses_explicit_key_when_provided() {
        // `api_key()` always wins; combined with
        // `skip_env_lookup(true)` the test is deterministic
        // even on dev machines that have `CWA_API_KEY`
        // exported.
        let connector = CwaConnector::builder()
            .base_url("https://example.test")
            .api_key("explicit-key")
            .auto_fetch_robots(false)
            .skip_env_lookup(true)
            .build()
            .await
            .unwrap();
        assert_eq!(connector.api_key.expose(), "explicit-key");
    }

    #[test]
    fn api_key_debug_is_redacted() {
        // The whole point of the newtype: a stray `{:?}`
        // in a log line CANNOT leak the key.
        let k = ApiKey("super-secret".into());
        let debug = format!("{k:?}");
        assert_eq!(debug, "ApiKey(<redacted>)");
        assert!(!debug.contains("super-secret"));
    }

    #[test]
    fn origin_key_includes_scheme_and_port() {
        let a = origin_key(&Url::parse("http://example.test:8001/foo").unwrap());
        let b = origin_key(&Url::parse("http://example.test:8002/foo").unwrap());
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn build_treats_robots_404_as_permissive() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/robots.txt"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let connector = CwaConnector::builder()
            .base_url(server.uri())
            .api_key("test-key")
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
        let err = CwaConnector::builder()
            .base_url(server.uri())
            .api_key("test-key")
            .throttle_ms(10)
            .build()
            .await
            .expect_err("503 should fail");
        assert!(matches!(err, BuildError::RobotsStatus { status: 503, .. }));
    }

    #[tokio::test]
    async fn build_error_invalid_url_carries_input_value() {
        let err = CwaConnector::builder()
            .base_url("not a url")
            .api_key("test-key")
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
    fn robots_parser_extracts_star_disallow() {
        let body = "User-agent: *\nDisallow: /admin\n";
        assert_eq!(parse_user_agent_star_disallow(body), vec!["/admin"]);
    }

    #[test]
    fn robots_parser_blank_line_terminates_group() {
        // The MOEA-side fix carried over: an empty `*`
        // group must not leak `*` into the next group.
        let body = "User-agent: *\n\nUser-agent: GoogleBot\nDisallow: /private\n";
        assert!(parse_user_agent_star_disallow(body).is_empty());
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
