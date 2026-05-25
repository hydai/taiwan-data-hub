//! `/api/v1/catalog/*` HTTP routes (#2.3).
//!
//! Public, read-only REST surface that mirrors the marketplace
//! YAML catalog (`config/domains.yaml`, `config/datasets.yaml`,
//! `config/collections.yaml`). Each request resolves i18n fields
//! against the requested locale and falls back to `zh-TW` per the
//! project's source-language policy.
//!
//! Why `/catalog/` and not the issue title's literal
//! `/api/v1/{domains,datasets,collections}`: `/api/v1/collections`
//! is already the user-owned-collections endpoint shipped in
//! #5a.4, and the `SvelteKit` bookmarks gateway calls it from the
//! frontend. Mounting two distinct concepts at the same path
//! would either break the existing client or require an awkward
//! per-method auth split. Namespacing the read-only catalog
//! under `/catalog/` keeps both surfaces stable and signals
//! "this is the public marketplace view" to consumers.
//!
//! Three list endpoints (`/domains`, `/datasets`, `/collections`)
//! each support offset/limit pagination, and the two detail
//! endpoints (`/datasets/{slug}`, `/collections/{slug}`) return
//! the matching resource or 404. Filters on `/datasets` mirror
//! the `DoD`: `domain`, `tier`, `license`, `format`. All handlers
//! are annotated with `#[utoipa::path]` so the `OpenAPI` document
//! at `/api/openapi.json` lists them automatically.

use std::sync::OnceLock;

use axum::Json;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use tools_data::domains::{Domain as SeedDomain, I18nText};
use utoipa::{IntoParams, ToSchema};

/// Embedded YAML sources. Same files the `SvelteKit` loaders read
/// via Vite's `?raw` import — keeping them in lockstep means the
/// REST API and the SSR pages can't show different catalogs.
///
/// Paths resolve at compile time from
/// `crates/gateway/src/marketplace_routes.rs` to the repo-root
/// `config/`. A renamed config file would fail the build.
///
/// Note: domains.yaml is parsed via `tools_data::domains::embedded`
/// so the gateway can't drift from the MCP `list_domains` tool's
/// view; only the dataset + collection embeds live here.
const DATASETS_YAML: &str = include_str!("../../../config/datasets.yaml");
const COLLECTIONS_YAML: &str = include_str!("../../../config/collections.yaml");

/// Fallback locale when the client omits `?lang=` or the requested
/// locale isn't in the i18n map. `zh-TW` is the source language
/// per CLAUDE.md.
const DEFAULT_LOCALE: &str = "zh-TW";

/// Default page size when the client omits `?limit=`. Sized to fit
/// every list in a single round-trip today (20 domains, ~60
/// datasets, 3 collections); clients that want pagination still
/// get cursored behavior via `offset`+`limit`.
const DEFAULT_LIMIT: u32 = 50;

/// Upper bound on `?limit=` per request. A hostile client cannot
/// ask for 1M rows; a friendly client that genuinely needs the
/// whole catalog just iterates a couple of pages.
const MAX_LIMIT: u32 = 200;

// ── Seed types (the YAML wire shape, parsed once) ─────────────────────

/// One dataset entry in `config/datasets.yaml`. Mirrors the
/// `SvelteKit` `Dataset` interface in
/// `web/src/lib/datasets/types.ts` field-for-field so the same
/// YAML serializes equally well on both sides.
#[derive(Debug, Clone, Deserialize)]
struct SeedDataset {
    slug: String,
    domain_slug: String,
    sort_order: i32,
    name: I18nText,
    description: I18nText,
    tier: String,
    format: String,
    license: String,
    source: SeedDatasetSource,
    updated: String,
    resources: Vec<SeedDatasetResource>,
}

#[derive(Debug, Clone, Deserialize)]
struct SeedDatasetSource {
    publisher: String,
    url: String,
    #[serde(default)]
    license_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SeedDatasetResource {
    kind: String,
    label: String,
    url: String,
}

/// One collection entry in `config/collections.yaml`.
#[derive(Debug, Clone, Deserialize)]
struct SeedCollection {
    slug: String,
    sort_order: i32,
    name: I18nText,
    curator_note: I18nText,
    anchor_datasets: Vec<String>,
}

// ── Response (resolved) types ─────────────────────────────────────────

/// Public view of one domain — i18n fields already resolved to
/// the requested locale.
#[derive(Debug, Serialize, ToSchema)]
pub struct DomainResource {
    pub slug: String,
    /// `"topical" | "meta" | "horizontal"`.
    pub kind: String,
    pub sort_order: i32,
    pub name: String,
    /// `null` when the seed entry omits `description`.
    pub description: Option<String>,
}

/// Public view of one dataset.
#[derive(Debug, Serialize, ToSchema)]
pub struct DatasetResource {
    pub slug: String,
    pub domain_slug: String,
    pub sort_order: i32,
    pub name: String,
    pub description: String,
    /// `"gold" | "silver" | "bronze"` (matches the YAML's
    /// editorial tiers; #2.8 will introduce the DB-derived
    /// score-based tiering).
    pub tier: String,
    /// `"csv" | "json" | "geojson" | "xlsx" | "parquet" | "xml"`.
    pub format: String,
    pub license: String,
    pub publisher: String,
    pub source_url: String,
    /// Canonical URL for the license document. Optional —
    /// not every license has a stable web home.
    pub license_url: Option<String>,
    /// Update cadence label: `"daily" | "weekly" | "monthly" | "quarterly" | "yearly"`.
    pub updated: String,
    pub resources: Vec<DatasetResourceLink>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DatasetResourceLink {
    /// `"download" | "api"`.
    pub kind: String,
    pub label: String,
    pub url: String,
}

/// Public view of one curated collection.
#[derive(Debug, Serialize, ToSchema)]
pub struct CollectionResource {
    pub slug: String,
    pub sort_order: i32,
    pub name: String,
    pub curator_note: String,
    /// Exactly 6 dataset slugs per the M2 #2.7 `DoD`; clients can
    /// dereference these via `GET /api/v1/catalog/datasets/{slug}`.
    pub anchor_datasets: Vec<String>,
}

/// Concrete schema for the `GET /api/v1/catalog/domains`
/// response body. Flat shape (data + offset/limit + total at
/// top level) so JSON clients don't need to deref a nested
/// `page` object to read counters.
///
/// Three concrete wrappers (one per resource) — `utoipa 5.x`
/// emits one `OpenAPI` `components/schemas` entry per concrete
/// type, not per generic instantiation, so wrapping the envelope
/// is the simplest way to keep the spec readable.
#[derive(Debug, Serialize, ToSchema)]
pub struct DomainListResponse {
    pub data: Vec<DomainResource>,
    /// Echo of the requested offset (defaults to 0).
    pub offset: u32,
    /// Echo of the effective limit after clamping (≤ `MAX_LIMIT`).
    pub limit: u32,
    /// Total number of items in the unpaginated result set
    /// after filtering. Lets clients know when to stop.
    pub total: u32,
}

/// Concrete schema for the `GET /api/v1/catalog/datasets`
/// response body. See [`DomainListResponse`] for the rationale.
#[derive(Debug, Serialize, ToSchema)]
pub struct DatasetListResponse {
    pub data: Vec<DatasetResource>,
    pub offset: u32,
    pub limit: u32,
    pub total: u32,
}

/// Concrete schema for the `GET /api/v1/catalog/collections`
/// response body. See [`DomainListResponse`] for the rationale.
#[derive(Debug, Serialize, ToSchema)]
pub struct CollectionListResponse {
    pub data: Vec<CollectionResource>,
    pub offset: u32,
    pub limit: u32,
    pub total: u32,
}

// ── Query-param structs ───────────────────────────────────────────────

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListQuery {
    /// IETF BCP-47 locale tag. Unknown locales fall back to
    /// `zh-TW`. Default: `zh-TW`.
    #[serde(default)]
    pub lang: Option<String>,
    /// Zero-based offset into the result set. Default `0`.
    #[serde(default)]
    pub offset: Option<u32>,
    /// Page size. Capped at `MAX_LIMIT` (200). Default `50`.
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DatasetListQuery {
    /// IETF BCP-47 locale tag. Unknown locales fall back to
    /// `zh-TW`. Default: `zh-TW`.
    #[serde(default)]
    pub lang: Option<String>,
    /// Filter by `domain_slug` (exact match).
    #[serde(default)]
    pub domain: Option<String>,
    /// Filter by tier (`gold | silver | bronze`).
    #[serde(default)]
    pub tier: Option<String>,
    /// Filter by license (exact string match — values are
    /// authored verbatim in the YAML).
    #[serde(default)]
    pub license: Option<String>,
    /// Filter by format (`csv | json | geojson | xlsx | parquet | xml`).
    #[serde(default)]
    pub format: Option<String>,
    /// Zero-based offset into the result set. Default `0`.
    #[serde(default)]
    pub offset: Option<u32>,
    /// Page size. Capped at `MAX_LIMIT` (200). Default `50`.
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LocaleQuery {
    /// IETF BCP-47 locale tag. Unknown locales fall back to
    /// `zh-TW`. Default: `zh-TW`.
    #[serde(default)]
    pub lang: Option<String>,
}

// ── Embedded caches ───────────────────────────────────────────────────

/// Cached, sort-stable view of `config/domains.yaml`.
fn domains_seed() -> &'static [SeedDomain] {
    // `tools_data::domains::embedded` already parses + sorts the
    // YAML once per process; reuse it so the gateway can't drift
    // from the MCP `list_domains` tool's view of the catalog.
    tools_data::domains::embedded()
}

/// Cached, sort-stable view of `config/datasets.yaml`. Sorted by
/// (`domain_slug` ASC, `sort_order` ASC, slug ASC) so list responses
/// have a stable order across requests regardless of the YAML
/// authoring order.
fn datasets_seed() -> &'static [SeedDataset] {
    static CACHE: OnceLock<Vec<SeedDataset>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let mut datasets: Vec<SeedDataset> = serde_yml::from_str(DATASETS_YAML)
                .expect("config/datasets.yaml must parse into Vec<SeedDataset>");
            datasets.sort_by(|a, b| {
                a.domain_slug
                    .cmp(&b.domain_slug)
                    .then(a.sort_order.cmp(&b.sort_order))
                    .then(a.slug.cmp(&b.slug))
            });
            datasets
        })
        .as_slice()
}

/// Cached, sort-stable view of `config/collections.yaml`. Sorted
/// by (`sort_order` ASC, slug ASC).
fn collections_seed() -> &'static [SeedCollection] {
    static CACHE: OnceLock<Vec<SeedCollection>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let mut collections: Vec<SeedCollection> = serde_yml::from_str(COLLECTIONS_YAML)
                .expect("config/collections.yaml must parse into Vec<SeedCollection>");
            collections.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.slug.cmp(&b.slug)));
            collections
        })
        .as_slice()
}

// ── Helpers ───────────────────────────────────────────────────────────

/// Resolve the request's locale string, defaulting to `zh-TW`.
/// `None` and the empty string both return `zh-TW` so a client
/// that sends `?lang=` (no value) still gets a usable response.
fn locale_for(lang: Option<&str>) -> &str {
    lang.map_or(DEFAULT_LOCALE, |raw| {
        if raw.is_empty() { DEFAULT_LOCALE } else { raw }
    })
}

/// Clamp a client-supplied `limit` into `[1, MAX_LIMIT]`. Zero
/// is bumped to 1 (a zero-length page is rarely what anyone
/// wants and a `Vec` slice with `..0` returns nothing useful).
fn effective_limit(requested: Option<u32>) -> u32 {
    requested.map_or(DEFAULT_LIMIT, |raw| raw.clamp(1, MAX_LIMIT))
}

/// Slice a sorted iterator into a single offset/limit page.
/// Returns `(page_items, total_unpaginated)`.
fn paginate<'a, T, F, R>(items: &'a [T], offset: u32, limit: u32, project: F) -> (Vec<R>, u32)
where
    F: Fn(&'a T) -> R,
{
    let total = u32::try_from(items.len()).unwrap_or(u32::MAX);
    let start = offset.min(total) as usize;
    let end = offset.saturating_add(limit).min(total) as usize;
    let page = items[start..end].iter().map(project).collect();
    (page, total)
}

fn project_domain(d: &SeedDomain, locale: &str) -> DomainResource {
    DomainResource {
        slug: d.slug.clone(),
        kind: d.kind.as_str().to_owned(),
        sort_order: d.sort_order,
        name: d.name.resolve(locale).to_owned(),
        description: d.description.as_ref().map(|t| t.resolve(locale).to_owned()),
    }
}

fn project_dataset(d: &SeedDataset, locale: &str) -> DatasetResource {
    DatasetResource {
        slug: d.slug.clone(),
        domain_slug: d.domain_slug.clone(),
        sort_order: d.sort_order,
        name: d.name.resolve(locale).to_owned(),
        description: d.description.resolve(locale).to_owned(),
        tier: d.tier.clone(),
        format: d.format.clone(),
        license: d.license.clone(),
        publisher: d.source.publisher.clone(),
        source_url: d.source.url.clone(),
        license_url: d.source.license_url.clone(),
        updated: d.updated.clone(),
        resources: d
            .resources
            .iter()
            .map(|r| DatasetResourceLink {
                kind: r.kind.clone(),
                label: r.label.clone(),
                url: r.url.clone(),
            })
            .collect(),
    }
}

fn project_collection(c: &SeedCollection, locale: &str) -> CollectionResource {
    CollectionResource {
        slug: c.slug.clone(),
        sort_order: c.sort_order,
        name: c.name.resolve(locale).to_owned(),
        curator_note: c.curator_note.resolve(locale).to_owned(),
        anchor_datasets: c.anchor_datasets.clone(),
    }
}

/// Apply the dataset filter quartet. Each `Option<&str>` is
/// short-circuited so the request that passes none of them
/// allocates nothing for filtering.
fn filter_datasets<'a>(
    pool: &'a [SeedDataset],
    domain: Option<&str>,
    tier: Option<&str>,
    license: Option<&str>,
    format: Option<&str>,
) -> Vec<&'a SeedDataset> {
    pool.iter()
        .filter(|d| domain.is_none_or(|v| d.domain_slug == v))
        .filter(|d| tier.is_none_or(|v| d.tier == v))
        .filter(|d| license.is_none_or(|v| d.license == v))
        .filter(|d| format.is_none_or(|v| d.format == v))
        .collect()
}

// ── Handlers ──────────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/api/v1/catalog/domains",
    tag = "catalog",
    description = "List the marketplace domains (the 20-row top-level dataset taxonomy).",
    params(ListQuery),
    responses(
        (status = 200, description = "Paginated list of domains", body = DomainListResponse),
    ),
)]
pub(crate) async fn list_domains(Query(q): Query<ListQuery>) -> Json<DomainListResponse> {
    let locale = locale_for(q.lang.as_deref());
    let offset = q.offset.unwrap_or(0);
    let limit = effective_limit(q.limit);
    let (data, total) = paginate(domains_seed(), offset, limit, |d| project_domain(d, locale));
    Json(DomainListResponse {
        data,
        offset,
        limit,
        total,
    })
}

#[utoipa::path(
    get,
    path = "/api/v1/catalog/datasets",
    tag = "catalog",
    description = "List marketplace datasets. Supports filtering by domain, tier, license, format.",
    params(DatasetListQuery),
    responses(
        (status = 200, description = "Paginated list of datasets", body = DatasetListResponse),
    ),
)]
pub(crate) async fn list_datasets(Query(q): Query<DatasetListQuery>) -> Json<DatasetListResponse> {
    let locale = locale_for(q.lang.as_deref());
    let offset = q.offset.unwrap_or(0);
    let limit = effective_limit(q.limit);
    let filtered = filter_datasets(
        datasets_seed(),
        q.domain.as_deref(),
        q.tier.as_deref(),
        q.license.as_deref(),
        q.format.as_deref(),
    );
    let (data, total) = paginate(&filtered, offset, limit, |d| project_dataset(d, locale));
    Json(DatasetListResponse {
        data,
        offset,
        limit,
        total,
    })
}

#[utoipa::path(
    get,
    path = "/api/v1/catalog/datasets/{slug}",
    tag = "catalog",
    description = "Fetch a single dataset by slug.",
    params(
        ("slug" = String, Path, description = "Dataset slug (kebab-case)"),
        LocaleQuery,
    ),
    responses(
        (status = 200, description = "The requested dataset", body = DatasetResource),
        (status = 404, description = "No dataset with the given slug"),
    ),
)]
pub(crate) async fn get_dataset(
    Path(slug): Path<String>,
    Query(q): Query<LocaleQuery>,
) -> Result<Json<DatasetResource>, StatusCode> {
    let locale = locale_for(q.lang.as_deref());
    datasets_seed()
        .iter()
        .find(|d| d.slug == slug)
        .map(|d| Json(project_dataset(d, locale)))
        .ok_or(StatusCode::NOT_FOUND)
}

#[utoipa::path(
    get,
    path = "/api/v1/catalog/collections",
    tag = "catalog",
    description = "List the curated collections (editorial dataset packs).",
    params(ListQuery),
    responses(
        (status = 200, description = "Paginated list of collections", body = CollectionListResponse),
    ),
)]
pub(crate) async fn list_collections(Query(q): Query<ListQuery>) -> Json<CollectionListResponse> {
    let locale = locale_for(q.lang.as_deref());
    let offset = q.offset.unwrap_or(0);
    let limit = effective_limit(q.limit);
    let (data, total) = paginate(collections_seed(), offset, limit, |c| {
        project_collection(c, locale)
    });
    Json(CollectionListResponse {
        data,
        offset,
        limit,
        total,
    })
}

#[utoipa::path(
    get,
    path = "/api/v1/catalog/collections/{slug}",
    tag = "catalog",
    description = "Fetch a single curated collection by slug.",
    params(
        ("slug" = String, Path, description = "Collection slug (kebab-case)"),
        LocaleQuery,
    ),
    responses(
        (status = 200, description = "The requested collection", body = CollectionResource),
        (status = 404, description = "No collection with the given slug"),
    ),
)]
pub(crate) async fn get_collection(
    Path(slug): Path<String>,
    Query(q): Query<LocaleQuery>,
) -> Result<Json<CollectionResource>, StatusCode> {
    let locale = locale_for(q.lang.as_deref());
    collections_seed()
        .iter()
        .find(|c| c.slug == slug)
        .map(|c| Json(project_collection(c, locale)))
        .ok_or(StatusCode::NOT_FOUND)
}

// ── Router ────────────────────────────────────────────────────────────

/// Build the `/api/v1/catalog/*` subrouter. Public + read-only;
/// no DB or auth state, so safe to mount in personal-mode too.
///
/// Mounted in `main.rs` alongside `config_router` under the IP
/// rate-limit layer. Each handler is a pure transform of the
/// embedded YAML cache; cold-start cost is the one-time
/// `serde_yml::from_str` per file.
pub fn router() -> axum::Router {
    axum::Router::new()
        .route("/api/v1/catalog/domains", axum::routing::get(list_domains))
        .route(
            "/api/v1/catalog/datasets",
            axum::routing::get(list_datasets),
        )
        .route(
            "/api/v1/catalog/datasets/{slug}",
            axum::routing::get(get_dataset),
        )
        .route(
            "/api/v1/catalog/collections",
            axum::routing::get(list_collections),
        )
        .route(
            "/api/v1/catalog/collections/{slug}",
            axum::routing::get(get_collection),
        )
}

/// One-shot warm-up of the three embedded caches. Called from
/// `main.rs` at boot so a malformed YAML panics before traffic
/// reaches the gateway, matching the same fail-fast pattern that
/// `tools_data::domains::embedded` provides for the MCP tools.
pub fn warm_seeds() {
    let _ = domains_seed();
    let _ = datasets_seed();
    let _ = collections_seed();
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use serde_json::Value;
    use tower::ServiceExt as _;

    /// Drive the router with a `Request` and parse the JSON body.
    async fn request_json(uri: &str) -> (StatusCode, Value) {
        let app = router();
        let resp = app
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 4 * 1024 * 1024).await.unwrap();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, body)
    }

    #[test]
    fn seed_caches_parse_and_have_expected_counts() {
        // 20 domains by design (CLAUDE.md §1.2), at least 3
        // curated collections (#2.7's seed) and at least one
        // dataset per anchor slug (asserted indirectly below).
        assert_eq!(domains_seed().len(), 20);
        assert!(
            collections_seed().len() >= 3,
            "expected at least 3 curated collections",
        );
        assert!(
            !datasets_seed().is_empty(),
            "expected at least one dataset seed",
        );
    }

    #[test]
    fn locale_for_defaults_to_zh_tw() {
        assert_eq!(locale_for(None), "zh-TW");
        assert_eq!(locale_for(Some("")), "zh-TW");
        assert_eq!(locale_for(Some("en")), "en");
        assert_eq!(locale_for(Some("ja")), "ja");
    }

    #[test]
    fn effective_limit_clamps_and_defaults() {
        assert_eq!(effective_limit(None), DEFAULT_LIMIT);
        assert_eq!(effective_limit(Some(0)), 1);
        assert_eq!(effective_limit(Some(10)), 10);
        assert_eq!(effective_limit(Some(MAX_LIMIT + 1)), MAX_LIMIT);
        assert_eq!(effective_limit(Some(u32::MAX)), MAX_LIMIT);
    }

    #[test]
    fn paginate_walks_pages_and_handles_overflow() {
        let items: Vec<u32> = (0..10).collect();
        let (page, total) = paginate(&items, 0, 3, |&n| n);
        assert_eq!(page, vec![0, 1, 2]);
        assert_eq!(total, 10);

        let (page, total) = paginate(&items, 3, 3, |&n| n);
        assert_eq!(page, vec![3, 4, 5]);
        assert_eq!(total, 10);

        // Offset past the end → empty page, total unchanged.
        let (page, total) = paginate(&items, 50, 3, |&n| n);
        assert!(page.is_empty());
        assert_eq!(total, 10);

        // Offset + limit > total → trimmed to remaining tail.
        let (page, total) = paginate(&items, 8, 5, |&n| n);
        assert_eq!(page, vec![8, 9]);
        assert_eq!(total, 10);
    }

    #[tokio::test]
    async fn list_domains_returns_all_twenty_under_default_paging() {
        let (status, body) = request_json("/api/v1/catalog/domains").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total"], 20);
        assert_eq!(body["offset"], 0);
        assert_eq!(body["limit"], DEFAULT_LIMIT);
        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 20);
        // Default locale is zh-TW; the first domain name should
        // be Chinese (the seed has zh-TW required on every row).
        let first = &data[0];
        assert!(!first["name"].as_str().unwrap().is_ascii());
        // Each domain has the documented shape.
        for entry in data {
            assert!(entry["slug"].is_string());
            assert!(["topical", "meta", "horizontal"].contains(&entry["kind"].as_str().unwrap()));
            assert!(entry["sort_order"].is_number());
            assert!(entry["name"].is_string());
        }
    }

    #[tokio::test]
    async fn list_domains_resolves_english_locale_when_requested() {
        let (status, body) = request_json("/api/v1/catalog/domains?lang=en").await;
        assert_eq!(status, StatusCode::OK);
        let first_name = body["data"][0]["name"].as_str().unwrap();
        // Every seeded domain ships an English name; we should see
        // ASCII letters not zh-TW characters in the en projection.
        assert!(
            first_name.chars().all(|c| c.is_ascii() || c == '&'),
            "expected ASCII English domain name, got {first_name:?}",
        );
    }

    #[tokio::test]
    async fn list_domains_falls_back_to_zh_tw_for_unknown_locale() {
        // Korean fallback path: zh-TW kept on every domain so any
        // unknown locale collapses back to the source language.
        let (status, body_unknown) = request_json("/api/v1/catalog/domains?lang=ko").await;
        assert_eq!(status, StatusCode::OK);
        let (_, body_zh) = request_json("/api/v1/catalog/domains?lang=zh-TW").await;
        assert_eq!(body_unknown["data"], body_zh["data"]);
    }

    #[tokio::test]
    async fn list_domains_offset_and_limit_paginate() {
        let (_, page1) = request_json("/api/v1/catalog/domains?offset=0&limit=5").await;
        let (_, page2) = request_json("/api/v1/catalog/domains?offset=5&limit=5").await;
        assert_eq!(page1["data"].as_array().unwrap().len(), 5);
        assert_eq!(page2["data"].as_array().unwrap().len(), 5);
        assert_eq!(page1["limit"], 5);
        assert_eq!(page2["offset"], 5);
        // Pages don't overlap.
        let p1_slugs: Vec<&str> = page1["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["slug"].as_str().unwrap())
            .collect();
        let p2_slugs: Vec<&str> = page2["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["slug"].as_str().unwrap())
            .collect();
        for s in &p1_slugs {
            assert!(!p2_slugs.contains(s), "slug {s:?} appeared on both pages",);
        }
    }

    #[tokio::test]
    async fn list_domains_clamps_oversized_limit() {
        let (_, body) =
            request_json(&format!("/api/v1/catalog/domains?limit={}", MAX_LIMIT * 10)).await;
        assert_eq!(body["limit"], MAX_LIMIT);
    }

    #[tokio::test]
    async fn list_datasets_default_returns_first_page_with_total() {
        let (status, body) = request_json("/api/v1/catalog/datasets?limit=3").await;
        assert_eq!(status, StatusCode::OK);
        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 3);
        assert!(body["total"].as_u64().unwrap() >= 3);
    }

    #[tokio::test]
    async fn list_datasets_filters_by_domain() {
        let (_, body) =
            request_json("/api/v1/catalog/datasets?domain=education-research&limit=50").await;
        let data = body["data"].as_array().unwrap();
        assert!(!data.is_empty(), "expected datasets in education-research");
        for entry in data {
            assert_eq!(entry["domain_slug"], "education-research");
        }
    }

    #[tokio::test]
    async fn list_datasets_filters_by_tier_format_license() {
        // tier=gold should only return gold-tier rows.
        let (_, body) = request_json("/api/v1/catalog/datasets?tier=gold&limit=100").await;
        for entry in body["data"].as_array().unwrap() {
            assert_eq!(entry["tier"], "gold");
        }
        // format=csv likewise.
        let (_, body) = request_json("/api/v1/catalog/datasets?format=csv&limit=100").await;
        for entry in body["data"].as_array().unwrap() {
            assert_eq!(entry["format"], "csv");
        }
        // Unknown filter values return an empty page (no enum
        // validation at the request layer — keeps the API forgiving
        // and matches the `SvelteKit` filter behaviour).
        let (_, body) = request_json("/api/v1/catalog/datasets?tier=purple").await;
        assert_eq!(body["data"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn get_dataset_returns_known_slug_and_404s_unknown() {
        // Pick the first dataset and round-trip it through the
        // detail endpoint. Avoids hard-coding a slug that future
        // YAML edits might rename.
        let first_slug = datasets_seed()[0].slug.clone();
        let (status, body) = request_json(&format!("/api/v1/catalog/datasets/{first_slug}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["slug"], first_slug.as_str());

        let (status, _) = request_json("/api/v1/catalog/datasets/this-slug-does-not-exist").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_collections_returns_seeded_packs() {
        let (status, body) = request_json("/api/v1/catalog/collections").await;
        assert_eq!(status, StatusCode::OK);
        let data = body["data"].as_array().unwrap();
        assert!(data.len() >= 3);
        for entry in data {
            assert!(entry["slug"].is_string());
            // M2 #2.7 DoD: exactly 6 anchor datasets per pack.
            assert_eq!(entry["anchor_datasets"].as_array().unwrap().len(), 6);
        }
    }

    #[tokio::test]
    async fn get_collection_returns_known_slug_and_404s_unknown() {
        let first_slug = collections_seed()[0].slug.clone();
        let (status, body) =
            request_json(&format!("/api/v1/catalog/collections/{first_slug}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["slug"], first_slug.as_str());

        let (status, _) =
            request_json("/api/v1/catalog/collections/this-pack-does-not-exist").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
