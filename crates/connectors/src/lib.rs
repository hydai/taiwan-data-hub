//! `SourceConnector` trait and implementations for upstream data sources.
//!
//! Connectors translate an upstream catalog (data.gov.tw, TWSE, MOEA, …)
//! into a uniform [`DatasetMetadata`] stream that the ETL pipeline can
//! upsert into Postgres. The trait is intentionally minimal: HTTP +
//! pagination is all #1.4a needs; DB writes (#1.4b), cron scheduling
//! (#1.4c), retry / DLQ (#5b.1) layer on top without changing the trait.
//!
//! The token returned by [`SourceConnector::source_id`] MUST match the
//! `datasets.source` CHECK constraint in `migrations/0001_init.sql`:
//! `data_gov_tw | twse | moea | cwa | fishery_moa | user_contrib`.

pub mod data_gov_tw;

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Stable token identifying an upstream source. Matches the
/// `datasets.source` SQL enum exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceId {
    DataGovTw,
    Twse,
    Moea,
    Cwa,
    FisheryMoa,
    UserContrib,
}

impl SourceId {
    /// SQL-side token. Returned by [`SourceConnector::source_id`].
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DataGovTw => "data_gov_tw",
            Self::Twse => "twse",
            Self::Moea => "moea",
            Self::Cwa => "cwa",
            Self::FisheryMoa => "fishery_moa",
            Self::UserContrib => "user_contrib",
        }
    }
}

impl std::fmt::Display for SourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One dataset as emitted by a connector — the **connector-side subset**
/// of what eventually lands in the `datasets` table.
///
/// The ETL/storage layer (#1.4b) is responsible for the columns this
/// struct deliberately omits:
///
/// - `domain_id` — resolved from [`Self::upstream_categories`] against
///   the 20-row `domains` table by the ETL mapping step.
/// - `tier` / `tier_score` — computed by the tier classifier described
///   in DESIGN.md §4.5.
/// - `schema_json` / `row_count_estimate` / `cached` / `cache_path` —
///   filled by the file-level crawl that follows the catalog crawl.
/// - `first_seen_at` — defaulted by Postgres at insert time
///   (`DEFAULT now()`).
/// - `last_modified_at` — the SQL column is `NOT NULL DEFAULT now()`.
///   This struct exposes `Option<DateTime<Utc>>` because upstream may
///   not carry it; the ETL layer is responsible for falling back to
///   `now()` (or the column default) when the option is `None`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DatasetMetadata {
    /// Upstream identifier — unique within the source. Used together
    /// with `source` to form the natural key in the `datasets` table.
    pub source_id: String,
    /// Kebab-case slug for marketplace URLs.
    pub slug: String,
    /// `{"zh-TW": "...", "en": "..."}` etc. `zh-TW` is required.
    pub title_i18n: BTreeMap<String, String>,
    /// Optional i18n description. Missing locales fall back to `zh-TW`
    /// per CLAUDE.md.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub description_i18n: BTreeMap<String, String>,
    /// SPDX-style or upstream-supplied license string.
    pub license: String,
    /// Publisher / responsible agency, if upstream reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    /// Human-readable update frequency hint (e.g. `"daily"`,
    /// `"每月更新"`). Unstructured by design — upstream catalogs vary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_frequency: Option<String>,
    /// Canonical upstream landing URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_url: Option<String>,
    /// Last modification time reported by upstream. `None` if upstream
    /// doesn't carry it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified_at: Option<DateTime<Utc>>,
    /// Raw upstream category strings. The ETL layer (#1.4b) maps these
    /// to one of the 20 internal `domains` rows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upstream_categories: Vec<String>,
}

/// Opaque pagination cursor. Each connector defines its own format
/// (offset, page number, continuation token, …); callers treat it as
/// a black box and just thread it back through.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cursor(String);

impl Cursor {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One page of dataset metadata, plus the cursor needed to fetch the
/// next page. `next` is `None` once upstream has been fully drained.
#[derive(Debug, Clone)]
pub struct Page {
    pub items: Vec<DatasetMetadata>,
    pub next: Option<Cursor>,
    /// Total result count reported by upstream, if any.
    pub total: Option<u64>,
}

/// Errors a connector can return. Concrete connectors surface their
/// own `reqwest::Error`s wrapped as [`Self::Transport`].
#[derive(Debug, Error)]
pub enum ConnectorError {
    #[error("HTTP transport error: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("HTTP {status}: {body}")]
    BadStatus { status: u16, body: String },
    #[error("could not parse upstream response: {0}")]
    Decode(String),
    /// Local misconfiguration — invalid URL, bad builder argument, etc.
    /// Distinct from [`Self::Decode`] which is always about *upstream's*
    /// response shape.
    #[error("connector misconfiguration: {0}")]
    Config(String),
    #[error("invalid cursor for {connector}: {reason}")]
    InvalidCursor {
        // Field name avoids `source` because thiserror reserves that for
        // error-chain wiring and would require `SourceId: std::error::Error`.
        connector: SourceId,
        reason: String,
    },
    #[error("unsupported feature: {0}")]
    Unsupported(&'static str),
}

/// HTTP cache cues a caller persists between crawls. The connector
/// translates these into conditional request headers
/// (`If-None-Match` / `If-Modified-Since`) on the FIRST page of a
/// walk so the server can short-circuit unchanged catalogs with 304.
///
/// Both fields are independent: a server may emit only one. Empty
/// cues (the `Default`) make the connector behave like the
/// unconditional pre-#1.4d.2 form — useful for the first-ever crawl
/// or for connectors / tests that don't bother with conditional
/// fetch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConditionalCues {
    /// Last-seen `ETag` header value (sent as `If-None-Match`).
    pub if_none_match: Option<String>,
    /// Last-seen `Last-Modified` header value (sent as
    /// `If-Modified-Since`). Format is whatever the server emitted —
    /// HTTP-date per RFC 7231 in well-behaved servers, but we pass it
    /// through verbatim so the round-trip is byte-stable.
    pub if_modified_since: Option<String>,
}

/// Outcome of a [`SourceConnector::list_datasets`] call.
///
/// `cues` are only meaningful when `cursor` is `None` (the first
/// page of a walk); subsequent paginated calls ignore them.
#[derive(Debug, Clone)]
pub enum ListResponse {
    /// Server returned a page. `fresh_cues` are extracted from the
    /// response headers and should be persisted for the next crawl.
    Modified {
        page: Page,
        fresh_cues: ConditionalCues,
    },
    /// Server returned `304 Not Modified`. Caller should skip the
    /// rest of the walk and refresh `last_seen_at` for the source
    /// (the cues themselves haven't changed).
    NotModified,
}

/// Drives the catalog walk for one upstream source.
///
/// Implementations are async + Send + Sync so the ETL scheduler in
/// #1.4c can run multiple connectors concurrently behind a single
/// tokio runtime.
#[async_trait]
pub trait SourceConnector: Send + Sync + 'static {
    /// Identifier matching the `datasets.source` SQL enum.
    fn source_id(&self) -> SourceId;

    /// Fetch the next page of dataset metadata. Pass `None` for the
    /// first call (along with the persisted `cues`); pass back the
    /// previous [`Page::next`] for subsequent calls (with default
    /// `cues` — they don't apply to mid-walk pages). A `None` return
    /// for [`Page::next`] means upstream is fully drained.
    ///
    /// When `cursor` is `None` and the server honours the conditional
    /// headers carried by `cues`, the connector may return
    /// [`ListResponse::NotModified`] — the catalog hasn't changed
    /// since the persisted cues were captured. Callers must treat
    /// this as a successful no-op crawl (refresh `last_seen_at`,
    /// don't touch ingested rows).
    async fn list_datasets(
        &self,
        cursor: Option<Cursor>,
        cues: &ConditionalCues,
    ) -> Result<ListResponse, ConnectorError>;
}

#[cfg(test)]
mod trait_tests {
    use super::*;

    #[test]
    fn source_id_tokens_match_sql_check_constraint() {
        // These strings are referenced by `datasets.source CHECK (... IN
        // ('data_gov_tw', 'twse', 'moea', 'cwa', 'fishery_moa',
        // 'user_contrib'))` in migrations/0001_init.sql. Drift between
        // the SQL and the Rust enum would surface at insert time as a
        // CHECK-constraint violation (Postgres error code 23514) — loud,
        // not silent — but the failure happens *after* the connector has
        // already done a full crawl. Catching the drift here at
        // `cargo test` lets CI fail fast.
        assert_eq!(SourceId::DataGovTw.as_str(), "data_gov_tw");
        assert_eq!(SourceId::Twse.as_str(), "twse");
        assert_eq!(SourceId::Moea.as_str(), "moea");
        assert_eq!(SourceId::Cwa.as_str(), "cwa");
        assert_eq!(SourceId::FisheryMoa.as_str(), "fishery_moa");
        assert_eq!(SourceId::UserContrib.as_str(), "user_contrib");
    }

    #[test]
    fn source_id_serde_roundtrips_via_snake_case() {
        let v = SourceId::DataGovTw;
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, r#""data_gov_tw""#);
        let back: SourceId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, v);
    }
}
