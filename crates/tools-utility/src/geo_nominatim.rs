//! Minimal Nominatim HTTP client shared by the `geo_geocode` and
//! `geo_reverse_geocode` MCP tools.
//!
//! Nominatim's public-usage policy is strict:
//!   - Mandatory descriptive `User-Agent` header
//!   - 1 request per second absolute maximum
//!   - No bulk geocoding (one-shot lookups only)
//!
//! We comply by:
//!   - Setting a project-identifying `User-Agent` on every request
//!   - Routing all calls through a process-wide async mutex that
//!     enforces ≥ 1 s between calls (slot-based — the next caller
//!     sleeps until `min_next_at` has passed before issuing)
//!
//! Operators that need higher throughput should self-host
//! Nominatim or use a commercial geocoder; the env var
//! `NOMINATIM_BASE_URL` lets a self-host swap in their endpoint.

use std::sync::OnceLock;
use std::time::Duration;

use reqwest::Client;
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::time::Instant;

/// Project-identifying user-agent. The Nominatim usage policy
/// requires "an HTTP referer and / or a valid HTTP user-agent
/// identifying the application", and rejects generic strings like
/// "curl/x.y" or "Mozilla/...".
const USER_AGENT: &str = concat!(
    "Taiwan-Data-Hub/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/hydai/taiwan-data-hub)"
);

/// Public default. Self-hosters override via `NOMINATIM_BASE_URL`.
pub const DEFAULT_BASE_URL: &str = "https://nominatim.openstreetmap.org";

/// Minimum spacing between consecutive requests, in line with the
/// Nominatim public-usage rule.
const MIN_INTERVAL: Duration = Duration::from_secs(1);

/// Per-request timeout — slow upstream shouldn't hang the whole MCP
/// dispatcher.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// One-time-initialised, process-wide HTTP client. Cheap to reuse,
/// keeps a connection pool, sets the user-agent + timeout once.
static CLIENT: OnceLock<Client> = OnceLock::new();

/// Slot-based throttle: holds the earliest time we may issue the
/// next request. Callers await the lock, sleep until the slot opens,
/// then issue and stamp `now + MIN_INTERVAL` into the slot before
/// releasing.
static THROTTLE: OnceLock<Mutex<Instant>> = OnceLock::new();

fn client() -> &'static Client {
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent(USER_AGENT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("reqwest client build must succeed")
    })
}

fn throttle() -> &'static Mutex<Instant> {
    THROTTLE.get_or_init(|| Mutex::new(Instant::now()))
}

fn base_url() -> String {
    // Trim trailing slashes from the override — `format!("{base}/search")`
    // would otherwise produce `https://host//search`, which some
    // reverse proxies normalise away but others reject outright.
    // Default value has no trailing slash, so this is a no-op for
    // the common case.
    let raw = std::env::var("NOMINATIM_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
    raw.trim_end_matches('/').to_string()
}

/// Wait until our throttle slot is free, then update the slot for
/// the next caller. We RESERVE the slot under the lock then DROP
/// the guard before sleeping — holding the mutex across the sleep
/// would serialise slot reservation (call A sleeps holding lock,
/// call B can't even decide when its turn is). With the early
/// drop, callers form a FIFO queue at the reservation step (which
/// is sub-microsecond) and then each sleeps independently until
/// its reserved instant.
async fn await_slot() {
    let reserved = {
        let mut next_at = throttle().lock().await;
        let now = Instant::now();
        let my_slot = (*next_at).max(now);
        *next_at = my_slot + MIN_INTERVAL;
        my_slot
        // Guard drops here at the end of the block.
    };
    let now = Instant::now();
    if reserved > now {
        tokio::time::sleep(reserved - now).await;
    }
}

#[derive(Debug, Error)]
pub enum NominatimError {
    #[error("nominatim request failed: {0}")]
    Request(String),
    #[error("nominatim response was not JSON: {0}")]
    Decode(String),
    #[error("nominatim returned HTTP {0}")]
    Status(u16),
}

/// `/search` — forward geocode (free-text → coordinates).
/// Returns up to `limit` results sorted by Nominatim's relevance.
pub async fn search(query: &str, limit: u32) -> Result<Vec<NominatimSearchHit>, NominatimError> {
    await_slot().await;
    let url = format!("{}/search", base_url());
    // Bind the limit string first so the slice literal below doesn't
    // borrow from a temporary; cleaner than relying on temporary-
    // lifetime extension, and refactor-safe.
    let limit_s = limit.to_string();
    let resp = client()
        .get(&url)
        .query(&[
            ("q", query),
            ("format", "jsonv2"),
            ("limit", limit_s.as_str()),
            ("addressdetails", "1"),
        ])
        // `without_url()` strips the request URL from the error chain
        // so query parameters can't leak into log lines.
        .send()
        .await
        .map_err(|e| NominatimError::Request(e.without_url().to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(NominatimError::Status(status.as_u16()));
    }
    let body = resp
        .bytes()
        .await
        .map_err(|e| NominatimError::Request(e.without_url().to_string()))?;
    serde_json::from_slice::<Vec<NominatimSearchHit>>(&body)
        .map_err(|e| NominatimError::Decode(e.to_string()))
}

/// `/reverse` — reverse geocode (coordinates → free-text + address parts).
pub async fn reverse(lat: f64, lon: f64) -> Result<NominatimReverseHit, NominatimError> {
    await_slot().await;
    let url = format!("{}/reverse", base_url());
    let resp = client()
        .get(&url)
        .query(&[
            ("lat", lat.to_string()),
            ("lon", lon.to_string()),
            ("format", "jsonv2".to_string()),
            ("addressdetails", "1".to_string()),
        ])
        .send()
        .await
        .map_err(|e| NominatimError::Request(e.without_url().to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(NominatimError::Status(status.as_u16()));
    }
    let body = resp
        .bytes()
        .await
        .map_err(|e| NominatimError::Request(e.without_url().to_string()))?;
    serde_json::from_slice::<NominatimReverseHit>(&body)
        .map_err(|e| NominatimError::Decode(e.to_string()))
}

#[derive(Debug, Deserialize, serde::Serialize)]
pub struct NominatimSearchHit {
    pub display_name: String,
    /// Stringly typed in Nominatim's JSON — keep the on-wire shape
    /// so callers can post-process without re-parsing.
    pub lat: String,
    pub lon: String,
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub class: Option<String>,
    #[serde(default)]
    pub addresstype: Option<String>,
    #[serde(default)]
    pub importance: Option<f64>,
    /// Structured address parts (city / county / postcode / etc.)
    /// when the upstream provides them. Surfaced because we already
    /// request `addressdetails=1` on the search call — without
    /// this field the extra payload was bandwidth wasted.
    #[serde(default)]
    pub address: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, serde::Serialize)]
pub struct NominatimReverseHit {
    pub display_name: String,
    pub lat: String,
    pub lon: String,
    #[serde(default)]
    pub address: Option<serde_json::Value>,
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub class: Option<String>,
}
