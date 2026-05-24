//! `/llms.txt` agent-discovery endpoint (#7.1).
//!
//! Renders the dataset catalog as a markdown document agents can read
//! to discover what's available. When the rendered body exceeds 5 MB
//! it splits into `/llms-index.txt` + numbered `/llms-page-N.txt`
//! pages with cross-links so an agent can still consume the catalog
//! without buffering arbitrarily large bodies. Per the llms.txt
//! convention (<https://llmstxt.org>), the file is markdown but
//! served as `text/markdown; charset=utf-8` with a strong `ETag` so
//! upstream CDNs and well-behaved agents can revalidate cheaply.
//!
//! The snapshot is built on demand the first time any of the three
//! routes is hit and cached in `Arc<RwLock<Option<…>>>`. A background
//! tokio task spawned at boot invalidates the snapshot every 24 h so
//! the document tracks the catalog without operators having to wire a
//! webhook. Storage writes can also call [`LlmsTxtCache::invalidate`]
//! directly when they want the next request to rebuild — that hook
//! lands separately and isn't required for the M7 `DoD`.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use storage::{DatasetSearcher, SearchHit, SearchParams, StorageError};
use tokio::sync::RwLock;

/// Soft cap per page. Picked just under the 5 MB ceiling the issue's
/// `DoD` specifies so a single oversized dataset description can't push
/// a page above the threshold. The actual page may be a few hundred
/// bytes over once the trailing cross-link is appended; staying well
/// below the hard ceiling keeps that within tolerance.
const DEFAULT_PAGE_BUDGET_BYTES: usize = 4_500_000;

/// Hard cap the `DoD` calls out — used as the budget the single-page
/// renderer compares against before deciding to paginate. Pages live
/// under [`DEFAULT_PAGE_BUDGET_BYTES`] so we never approach this
/// number in the paginated path; the constant exists so the threshold
/// the issue promises is named in code.
const DEFAULT_SINGLE_PAGE_HARD_CAP_BYTES: usize = 5 * 1024 * 1024;

/// Pagination thresholds. Production uses [`Limits::default`]; tests
/// shrink the numbers so they can exercise the paginated path
/// without rendering tens of megabytes of fixture data.
#[derive(Debug, Clone, Copy)]
struct Limits {
    /// Per-page byte budget — pages stop accepting more entries once
    /// the cumulative size would exceed this number.
    page_budget: usize,
    /// Switch-over threshold: a single-page render under this number
    /// of bytes is served as-is at `/llms.txt`; over it, the renderer
    /// paginates.
    single_page_cap: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            page_budget: DEFAULT_PAGE_BUDGET_BYTES,
            single_page_cap: DEFAULT_SINGLE_PAGE_HARD_CAP_BYTES,
        }
    }
}

/// Storage-side `search_datasets` clamps `limit` to 100. Mirroring
/// the constant here keeps the loop honest if the storage cap ever
/// moves — the builder would over-fetch instead of silently dropping
/// the tail.
const SEARCH_PAGE_SIZE: u32 = 100;

/// `Cache-Control` value applied to every response. One-hour public
/// cache balances "agents see fresh data" against "edge caches do
/// useful work"; the strong `ETag` makes the revalidation path cheap
/// when the underlying snapshot hasn't changed.
const CACHE_CONTROL: &str = "public, max-age=3600";

/// MIME type for the body. llms.txt is markdown by convention; some
/// agents look for `text/markdown` specifically, so we send that
/// rather than the more conservative `text/plain`. UTF-8 is mandatory
/// because most catalog descriptions contain CJK characters.
const CONTENT_TYPE: &str = "text/markdown; charset=utf-8";

/// Default nightly refresh cadence — the `DoD` calls for "regenerated
/// nightly". 24 h matches what an operator reading the docstring
/// expects without needing to remember the exact number.
const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Snapshot of the rendered llms.txt corpus. `pages.len() == 1` means
/// the catalog fit under the size budget so `/llms.txt` serves that
/// single page directly; `> 1` means we paginated and `/llms.txt`
/// redirects callers to the index.
#[derive(Debug, Clone)]
struct Snapshot {
    pages: Vec<String>,
    /// Index page rendered alongside the data pages so the index
    /// route serves a precomputed body rather than re-building on
    /// each request. `None` only in the single-page case.
    index: Option<String>,
    /// `SHA-256` of every page concatenated. Distinct `ETag`s between
    /// snapshots even when only one page changed because any
    /// content change shifts the digest.
    etag: String,
    generated_at: DateTime<Utc>,
}

impl Snapshot {
    fn is_paginated(&self) -> bool {
        self.pages.len() > 1
    }
}

/// Errors emitted while building the snapshot. Distinct from the
/// HTTP-level error (which collapses everything to a 500) so the
/// background refresh task can log the specific failure cause.
#[derive(Debug, thiserror::Error)]
pub enum LlmsTxtError {
    #[error("storage: {0}")]
    Storage(#[from] StorageError),
}

/// Read-side view used by the cache to fetch catalog pages. Reusing
/// [`DatasetSearcher`] keeps the schema knowledge centralised in the
/// storage layer — there's only one place that maps `datasets` rows
/// to a flat hit shape with i18n already resolved.
pub type DatasetSource = Arc<dyn DatasetSearcher>;

/// Site metadata embedded in the rendered output. `public_base_url`
/// drives the cross-link URLs the index page emits; tests inject a
/// fixed value so snapshots stay deterministic.
#[derive(Debug, Clone)]
pub struct LlmsTxtMeta {
    pub title: String,
    pub tagline: String,
    pub public_base_url: String,
}

impl LlmsTxtMeta {
    /// Default values used when the gateway boots without explicit
    /// configuration — `LLMS_TXT_BASE_URL` env var overrides.
    /// Keeping the default in code (rather than panicking on a
    /// missing env var) means the route works on a fresh laptop
    /// without any setup.
    pub fn defaults() -> Self {
        Self {
            title: "Taiwan Data Hub".to_string(),
            tagline: "Open Taiwan public data, exposed to AI agents via MCP.".to_string(),
            public_base_url: "https://taiwan-data-hub.example".to_string(),
        }
    }
}

/// Cache + builder for the rendered snapshot. Holding the source +
/// metadata behind `Arc` lets the background refresh task and every
/// HTTP handler share a single cache instance via clone.
///
/// The snapshot itself is wrapped in `Arc<Snapshot>` so the hot read
/// path doesn't deep-clone potentially multi-MB `Vec<String>` pages
/// on every request — handlers grab a refcount under the read lock
/// and the inner allocations stay shared.
#[derive(Clone)]
pub struct LlmsTxtCache {
    source: DatasetSource,
    meta: LlmsTxtMeta,
    snapshot: Arc<RwLock<Option<Arc<Snapshot>>>>,
}

impl LlmsTxtCache {
    pub fn new(source: DatasetSource, meta: LlmsTxtMeta) -> Self {
        Self {
            source,
            meta,
            snapshot: Arc::new(RwLock::new(None)),
        }
    }

    /// Drop the cached snapshot so the next request rebuilds. Cheap —
    /// just clears the slot under the write lock. The actual rebuild
    /// is lazy because nothing forces work onto the background task's
    /// schedule.
    ///
    /// Today only the unit tests call this; a future ETL-driven
    /// invalidation hook (so `upsert_dataset` can flush the cache
    /// without waiting for the nightly tick) is tracked separately.
    #[allow(dead_code)]
    pub async fn invalidate(&self) {
        *self.snapshot.write().await = None;
    }

    /// Build a fresh snapshot from the dataset source. Called both
    /// lazily (on first request after `invalidate`) and proactively
    /// (by the background refresh task once per day). Returns an
    /// `Arc` so the result can be installed into the cache without
    /// re-allocating the page strings.
    async fn build(&self) -> Result<Arc<Snapshot>, LlmsTxtError> {
        let hits = self.fetch_all_hits().await?;
        let snapshot = render_snapshot(&self.meta, &hits, &Limits::default());
        Ok(Arc::new(snapshot))
    }

    /// Walk the catalog in `SEARCH_PAGE_SIZE` strides until the
    /// storage layer reports no `next_offset`. Filters are all `None`
    /// so the result is every dataset in the catalog. Locale is
    /// `zh-TW` per CLAUDE.md's fallback chain — the source language
    /// is what the document is canonically authored in.
    async fn fetch_all_hits(&self) -> Result<Vec<SearchHit>, LlmsTxtError> {
        let mut all = Vec::new();
        let mut offset: u32 = 0;
        loop {
            let params = SearchParams {
                limit: SEARCH_PAGE_SIZE,
                offset,
                locale: Some("zh-TW".to_string()),
                ..Default::default()
            };
            let page = self.source.search_datasets(params).await?;
            let len = page.hits.len();
            all.extend(page.hits);
            match page.next_offset {
                Some(next) => offset = next,
                None => break,
            }
            if len == 0 {
                // Defensive: storage should set `next_offset = None`
                // on the final partial page, but if a stub returns
                // an empty page with a non-None offset we'd loop
                // forever. Treat empty as terminal regardless.
                break;
            }
        }
        Ok(all)
    }

    /// Return the current snapshot, building it on demand if absent.
    /// Two-phase locking: first try the read lock so the hot path
    /// stays read-only and only bumps an `Arc` refcount; rebuild
    /// only when the slot is empty.
    async fn get_or_build(&self) -> Result<Arc<Snapshot>, LlmsTxtError> {
        if let Some(snap) = self.snapshot.read().await.clone() {
            return Ok(snap);
        }
        // Upgrade to write lock. Re-check under the lock to avoid a
        // double-build when two requests race on a cold cache.
        let mut slot = self.snapshot.write().await;
        if let Some(snap) = slot.clone() {
            return Ok(snap);
        }
        let snap = self.build().await?;
        *slot = Some(snap.clone());
        Ok(snap)
    }

    /// Spawn the nightly refresh loop. Hands the join handle back so
    /// the caller can abort it on shutdown if it cares; today the
    /// gateway is happy to let the task die with the process.
    ///
    /// **Failure mode**: the loop builds the new snapshot *first*
    /// and only swaps it into the cache on success, so a transient
    /// upstream blip can't degrade an otherwise-good snapshot to a
    /// 500-serving cold state. The old snapshot keeps serving
    /// requests until the next successful refresh.
    pub fn spawn_refresh_task(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(DEFAULT_REFRESH_INTERVAL);
            // First tick fires immediately; skip it because the
            // cache is either fresh from boot or about to be built
            // on the first request. We want the *next* tick to be
            // the daily refresh, not "rebuild immediately".
            ticker.tick().await;
            loop {
                ticker.tick().await;
                match self.build().await {
                    Ok(snap) => {
                        let pages = snap.pages.len();
                        let bytes: usize = snap.pages.iter().map(String::len).sum();
                        // Atomic swap: the new snapshot is fully
                        // built before we touch the cache slot, so
                        // concurrent HTTP requests during the
                        // refresh window keep serving the previous
                        // snapshot. No cold window.
                        *self.snapshot.write().await = Some(snap);
                        tracing::info!(pages, bytes, "llms.txt snapshot refreshed");
                    }
                    Err(e) => {
                        // Keep serving the last-known-good
                        // snapshot — an upstream blip shouldn't
                        // demote the cache to a cold state. Log so
                        // operators see the failed refresh.
                        tracing::warn!(error = %e, "llms.txt refresh failed; keeping previous snapshot");
                    }
                }
            }
        })
    }
}

impl std::fmt::Debug for LlmsTxtCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmsTxtCache")
            .field("meta", &self.meta)
            .finish_non_exhaustive()
    }
}

/// Pure rendering: build the full snapshot from already-fetched hits.
/// Lifted out of [`LlmsTxtCache`] so unit tests can drive the
/// pagination + cross-link logic without any tokio runtime.
fn render_snapshot(meta: &LlmsTxtMeta, hits: &[SearchHit], limits: &Limits) -> Snapshot {
    let single_page = render_single_page(meta, hits);
    if single_page.len() <= limits.single_page_cap {
        let etag = etag_for(&[&single_page]);
        return Snapshot {
            pages: vec![single_page],
            index: None,
            etag,
            generated_at: Utc::now(),
        };
    }
    let pages = paginate_hits(meta, hits, limits);
    let index = render_index(meta, pages.len());
    let mut all = vec![index.clone()];
    all.extend(pages.iter().cloned());
    let etag = etag_for(&all.iter().map(String::as_str).collect::<Vec<_>>());
    Snapshot {
        pages,
        index: Some(index),
        etag,
        generated_at: Utc::now(),
    }
}

/// Render the whole catalog into a single markdown document. Used in
/// the common case (catalog ≤ 5 MB) and as the building block the
/// paginated path slices.
fn render_single_page(meta: &LlmsTxtMeta, hits: &[SearchHit]) -> String {
    let mut out = String::with_capacity(estimate_bytes(hits.len()));
    write_header(&mut out, meta);
    out.push_str("\n## Datasets\n\n");
    for hit in hits {
        write_hit(&mut out, meta, hit);
    }
    out
}

/// Split the catalog across pages capped at [`Limits::page_budget`].
/// Each page is a standalone markdown document — header + section
/// title + dataset entries — so an agent that fetches only one page
/// still gets a usable, self-describing fragment. The index page
/// rendered separately cross-links the lot.
fn paginate_hits(meta: &LlmsTxtMeta, hits: &[SearchHit], limits: &Limits) -> Vec<String> {
    let mut pages = Vec::new();
    let mut current = String::with_capacity(limits.page_budget / 4);
    let mut page_index = 1usize;
    start_page(&mut current, meta, page_index);

    for hit in hits {
        let mut entry = String::new();
        write_hit(&mut entry, meta, hit);
        if current.len() + entry.len() > limits.page_budget && current_has_any_entries(&current) {
            finalise_page(&mut current, meta, page_index);
            pages.push(std::mem::take(&mut current));
            page_index += 1;
            start_page(&mut current, meta, page_index);
        }
        current.push_str(&entry);
    }
    finalise_page(&mut current, meta, page_index);
    pages.push(current);
    pages
}

/// Marker the page-start template ends with so [`paginate_hits`] can
/// tell "header only" from "header + at least one entry" without an
/// extra counter. The literal `"## Datasets"` line appears exactly
/// once per page.
fn current_has_any_entries(buf: &str) -> bool {
    // After `start_page` writes the header and "## Datasets" line,
    // any subsequent dataset entry appends a `- [` bullet. Detecting
    // that bullet avoids splitting on a page that has nothing on it
    // yet — preventing an empty trailing page when a single dataset's
    // description happens to push us over the limit on its own.
    buf.contains("\n- [")
}

fn start_page(out: &mut String, meta: &LlmsTxtMeta, page_number: usize) {
    write_header(out, meta);
    let _ = write!(
        out,
        "\n*Page {page_number} of the paginated catalog.*\n\n## Datasets\n\n",
    );
}

fn finalise_page(out: &mut String, meta: &LlmsTxtMeta, page_number: usize) {
    let base = &meta.public_base_url;
    out.push_str("\n---\n\n");
    let _ = write!(out, "Catalog index: <{base}/llms-index.txt>. ");
    if page_number > 1 {
        let prev = page_number - 1;
        let _ = write!(out, "Previous page: <{base}/llms-page-{prev}.txt>. ");
    }
    let next = page_number + 1;
    let _ = writeln!(
        out,
        "Next page: <{base}/llms-page-{next}.txt> (404 marks the end).",
    );
}

fn render_index(meta: &LlmsTxtMeta, page_count: usize) -> String {
    let base = &meta.public_base_url;
    let mut out = String::with_capacity(1024 + page_count * 80);
    write_header(&mut out, meta);
    let _ = write!(
        out,
        "\nThe full catalog is paginated across **{page_count}** pages because the rendered document exceeds 5 MB.\n\n## Pages\n\n",
    );
    for n in 1..=page_count {
        let _ = writeln!(
            out,
            "- [Page {n}](/llms-page-{n}.txt) — <{base}/llms-page-{n}.txt>",
        );
    }
    out
}

fn write_header(out: &mut String, meta: &LlmsTxtMeta) {
    let title = &meta.title;
    let tagline = &meta.tagline;
    let _ = writeln!(out, "# {title}\n");
    let _ = writeln!(out, "> {tagline}");
}

fn write_hit(out: &mut String, meta: &LlmsTxtMeta, hit: &SearchHit) {
    let base = &meta.public_base_url;
    let slug = &hit.slug;
    let title = escape_markdown_inline(&hit.title);
    let domain = &hit.domain_slug;
    let tier = &hit.tier;
    let license = &hit.license;
    let _ = write!(
        out,
        "- [{title}]({base}/datasets/{slug}) — `{domain}` · `{tier}` · {license}",
    );
    if let Some(publisher) = &hit.publisher {
        let _ = write!(out, " · {publisher}");
    }
    out.push('\n');
    if let Some(description) = &hit.description {
        let trimmed = truncate_description(description);
        if !trimmed.is_empty() {
            let desc = escape_markdown_inline(&trimmed);
            let _ = writeln!(out, "  - {desc}");
        }
    }
}

/// Cap descriptions at 280 chars so a pathologically long entry can't
/// dominate the page. Cut at a char boundary, suffix with `…` so
/// agents see the truncation. The cap is generous enough that most
/// real descriptions land untouched.
fn truncate_description(s: &str) -> String {
    const MAX_CHARS: usize = 280;
    let trimmed = s.trim();
    let char_count = trimmed.chars().count();
    if char_count <= MAX_CHARS {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(MAX_CHARS).collect();
    out.push('…');
    out
}

/// Escape characters that would otherwise break the markdown shape
/// of the rendered list — pipes and backticks land inside code spans
/// and titles. Newlines collapse to spaces so a description with hard
/// breaks doesn't split a single bullet across multiple list items.
fn escape_markdown_inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\n' | '\r' => out.push(' '),
            '`' => out.push('\''),
            '|' => out.push('/'),
            _ => out.push(c),
        }
    }
    out
}

/// Very rough estimate of the byte budget the renderer needs up
/// front. Helps `String::with_capacity` avoid the worst of the
/// re-allocation churn on large catalogs.
fn estimate_bytes(hit_count: usize) -> usize {
    // ~256 bytes per entry + 512 bytes of header / footer slop.
    hit_count.saturating_mul(256).saturating_add(512)
}

/// Strong `ETag` derived from every page's bytes. Truncated to 16 hex
/// chars (64 bits) which keeps the header short while still leaving
/// collision odds negligible at the scale of "one snapshot per day".
fn etag_for(pages: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for page in pages {
        hasher.update(page.as_bytes());
        hasher.update([0u8]); // separator so two adjacent pages can't merge into one input
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(18);
    hex.push('"');
    for byte in digest.iter().take(8) {
        let _ = write!(hex, "{byte:02x}");
    }
    hex.push('"');
    hex
}

/// Build the axum subrouter mounting all three routes. The caller
/// merges this into the top-level router; the cache state is shared
/// across handlers via `with_state`.
pub fn router(cache: Arc<LlmsTxtCache>) -> Router {
    Router::new()
        .route("/llms.txt", get(handler_root))
        .route("/llms-index.txt", get(handler_index))
        .route("/llms-page-{n}.txt", get(handler_page))
        .with_state(cache)
}

async fn handler_root(State(cache): State<Arc<LlmsTxtCache>>, headers: HeaderMap) -> Response {
    match cache.get_or_build().await {
        Ok(snap) => {
            if let Some(resp) = not_modified_if_match(&headers, &snap.etag) {
                return resp;
            }
            let body = if snap.is_paginated() {
                snap.index.clone().unwrap_or_default()
            } else {
                snap.pages.first().cloned().unwrap_or_default()
            };
            success_response(body, &snap.etag, snap.generated_at)
        }
        Err(e) => error_response(&e),
    }
}

async fn handler_index(State(cache): State<Arc<LlmsTxtCache>>, headers: HeaderMap) -> Response {
    match cache.get_or_build().await {
        Ok(snap) => {
            if let Some(resp) = not_modified_if_match(&headers, &snap.etag) {
                return resp;
            }
            // Even when the catalog fits in one page we serve a
            // minimal index that points back at /llms.txt. That
            // keeps the URL discoverable for clients that always
            // try the index first.
            let body = snap
                .index
                .clone()
                .unwrap_or_else(|| render_single_page_index(&cache.meta));
            success_response(body, &snap.etag, snap.generated_at)
        }
        Err(e) => error_response(&e),
    }
}

async fn handler_page(
    State(cache): State<Arc<LlmsTxtCache>>,
    Path(n): Path<usize>,
    headers: HeaderMap,
) -> Response {
    if n == 0 {
        return (StatusCode::NOT_FOUND, "page numbers start at 1").into_response();
    }
    match cache.get_or_build().await {
        Ok(snap) => {
            if let Some(resp) = not_modified_if_match(&headers, &snap.etag) {
                return resp;
            }
            match snap.pages.get(n - 1) {
                Some(body) => success_response(body.clone(), &snap.etag, snap.generated_at),
                None => (StatusCode::NOT_FOUND, "no such page").into_response(),
            }
        }
        Err(e) => error_response(&e),
    }
}

fn not_modified_if_match(headers: &HeaderMap, etag: &str) -> Option<Response> {
    let inm = headers.get(header::IF_NONE_MATCH)?.to_str().ok()?;
    if !if_none_match_matches(inm, etag) {
        return None;
    }
    let mut resp = StatusCode::NOT_MODIFIED.into_response();
    resp.headers_mut()
        .insert(header::ETAG, HeaderValue::from_str(etag).ok()?);
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL),
    );
    Some(resp)
}

/// RFC 9110 §13.1.2-compliant `If-None-Match` comparison.
///
/// The header is a comma-separated list of entity tags, with the
/// special value `*` matching *any* current representation. Each
/// entry may be prefixed with `W/` for a weak validator — for
/// `If-None-Match` weak comparison is the prescribed mode, so we
/// strip the prefix before comparing. Our stored `etag` is always
/// strong (no `W/`), so the comparison reduces to literal equality
/// of the quoted-string portion.
fn if_none_match_matches(header_value: &str, etag: &str) -> bool {
    for raw in header_value.split(',') {
        let entry = raw.trim();
        if entry.is_empty() {
            continue;
        }
        if entry == "*" {
            return true;
        }
        let stripped = entry.strip_prefix("W/").unwrap_or(entry);
        if stripped == etag {
            return true;
        }
    }
    false
}

fn success_response(body: String, etag: &str, generated_at: DateTime<Utc>) -> Response {
    let mut resp = (StatusCode::OK, body).into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(CONTENT_TYPE));
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL),
    );
    if let Ok(v) = HeaderValue::from_str(etag) {
        h.insert(header::ETAG, v);
    }
    // RFC 7232 `Last-Modified` complements the strong ETag: clients
    // that can't process the opaque ETag still get a coarse
    // freshness signal they can use for `If-Modified-Since`. RFC
    // 9110 prescribes the "IMF-fixdate" format (`%a, %d %b %Y %T GMT`),
    // which is what `chrono::DateTime::to_rfc2822` emits with the
    // exception that the latter uses `+0000` instead of `GMT`.
    // Substitute the zone suffix so the header is spec-correct.
    let rfc2822 = generated_at.to_rfc2822();
    let imf_fixdate = rfc2822.replace("+0000", "GMT");
    if let Ok(v) = HeaderValue::from_str(&imf_fixdate) {
        h.insert(header::LAST_MODIFIED, v);
    }
    resp
}

fn error_response(err: &LlmsTxtError) -> Response {
    tracing::warn!(error = %err, "llms.txt render failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "llms.txt render failed; see gateway logs",
    )
        .into_response()
}

/// Minimal stand-in index for the case where the catalog fits in a
/// single page but a client still asks for `/llms-index.txt`. Keeps
/// the contract that the index route always returns something
/// useful, without artificially paginating below the threshold.
fn render_single_page_index(meta: &LlmsTxtMeta) -> String {
    let base = &meta.public_base_url;
    let mut out = String::with_capacity(256);
    write_header(&mut out, meta);
    let _ = writeln!(
        out,
        "\nThe full catalog is small enough to fit in one page: <{base}/llms.txt>",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;
    use storage::SearchPage;
    use uuid::Uuid;

    #[derive(Default)]
    struct StubSearcher {
        responses: Mutex<Vec<SearchPage>>,
    }

    impl StubSearcher {
        fn with_pages(pages: Vec<SearchPage>) -> Self {
            Self {
                responses: Mutex::new(pages),
            }
        }
    }

    #[async_trait]
    impl DatasetSearcher for StubSearcher {
        async fn search_datasets(&self, _params: SearchParams) -> Result<SearchPage, StorageError> {
            let mut q = self.responses.lock().unwrap();
            Ok(if q.is_empty() {
                SearchPage {
                    hits: vec![],
                    next_offset: None,
                }
            } else {
                q.remove(0)
            })
        }
    }

    fn meta() -> LlmsTxtMeta {
        LlmsTxtMeta {
            title: "Test Hub".into(),
            tagline: "tagline".into(),
            public_base_url: "https://example.test".into(),
        }
    }

    fn fixture_hit(slug: &str, description: Option<&str>) -> SearchHit {
        SearchHit {
            id: Uuid::nil(),
            slug: slug.to_owned(),
            title: format!("{slug} title"),
            description: description.map(str::to_owned),
            domain_slug: "environment".to_owned(),
            tier: "bronze".to_owned(),
            license: "CC-BY-4.0".to_owned(),
            publisher: Some("Agency".to_owned()),
        }
    }

    /// Test-only thresholds — small enough that a handful of fixture
    /// hits crosses the single-page cap and exercises the paginated
    /// path without rendering megabytes of fixture text.
    fn small_limits() -> Limits {
        Limits {
            page_budget: 2_000,
            single_page_cap: 4_000,
        }
    }

    #[test]
    fn single_page_render_contains_every_hit_and_header() {
        let hits = vec![
            fixture_hit("air-quality", Some("sensor PM2.5 readings")),
            fixture_hit("forest-land", None),
        ];
        let snap = render_snapshot(&meta(), &hits, &Limits::default());
        assert_eq!(snap.pages.len(), 1, "small catalog stays single-page");
        assert!(snap.index.is_none());
        let body = &snap.pages[0];
        assert!(body.starts_with("# Test Hub"));
        assert!(body.contains("[air-quality title](https://example.test/datasets/air-quality)"));
        assert!(body.contains("[forest-land title](https://example.test/datasets/forest-land)"));
        assert!(body.contains("sensor PM2.5 readings"));
    }

    #[test]
    fn paginates_when_over_hard_cap() {
        // Use the small fixture limits so we don't have to render a
        // real 5 MB document just to cross the threshold. 30 entries
        // × ~200 bytes (after description truncation) ≈ 6 KB total —
        // ample to clear the 4 KB single-page cap.
        let limits = small_limits();
        let mut hits = Vec::new();
        for i in 0..30 {
            hits.push(fixture_hit(
                &format!("ds-{i:04}"),
                Some("sensor reading description that survives truncation"),
            ));
        }
        let snap = render_snapshot(&meta(), &hits, &limits);
        assert!(
            snap.pages.len() >= 2,
            "expected pagination, got {} pages",
            snap.pages.len()
        );
        assert!(snap.index.is_some());
        let index = snap.index.unwrap();
        assert!(index.contains(&format!("Page {}", snap.pages.len())));
        for page in &snap.pages {
            assert!(
                page.len() <= limits.page_budget + 4096,
                "page size {} exceeded budget",
                page.len()
            );
        }
    }

    #[test]
    fn truncate_description_caps_long_strings() {
        let long = "字".repeat(500);
        let cut = truncate_description(&long);
        assert!(cut.ends_with('…'));
        assert_eq!(cut.chars().count(), 281, "max-chars + ellipsis");
    }

    #[test]
    fn escape_inline_collapses_newlines_and_backticks() {
        let escaped = escape_markdown_inline("line1\nline2 `code` |pipe|");
        assert_eq!(escaped, "line1 line2 'code' /pipe/");
    }

    #[test]
    fn if_none_match_wildcard_matches_any_etag() {
        assert!(if_none_match_matches("*", "\"abc123\""));
        assert!(if_none_match_matches(" * ", "\"abc123\""));
    }

    #[test]
    fn if_none_match_handles_comma_list_and_weak_prefix() {
        let stored = "\"abc\"";
        assert!(if_none_match_matches("\"abc\"", stored));
        assert!(if_none_match_matches("\"xyz\", \"abc\"", stored));
        assert!(if_none_match_matches("W/\"abc\"", stored));
        assert!(if_none_match_matches("\"zzz\", W/\"abc\"", stored));
        assert!(!if_none_match_matches("\"xyz\"", stored));
        // Substring of the stored etag must NOT match — guards the
        // pre-fix regression where the implementation used a naive
        // `inm.contains(etag)` check.
        assert!(!if_none_match_matches("\"ab\"", stored));
    }

    #[test]
    fn if_none_match_ignores_empty_entries() {
        assert!(if_none_match_matches(",,\"abc\",", "\"abc\""));
        assert!(!if_none_match_matches(",,", "\"abc\""));
    }

    #[test]
    fn etag_changes_when_content_changes() {
        let a = etag_for(&["page-1"]);
        let b = etag_for(&["page-1", "page-2"]);
        let c = etag_for(&["different"]);
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[tokio::test]
    async fn cache_walks_all_search_pages() {
        let source: DatasetSource = Arc::new(StubSearcher::with_pages(vec![
            SearchPage {
                hits: vec![fixture_hit("a", None)],
                next_offset: Some(1),
            },
            SearchPage {
                hits: vec![fixture_hit("b", None)],
                next_offset: Some(2),
            },
            SearchPage {
                hits: vec![fixture_hit("c", None)],
                next_offset: None,
            },
        ]));
        let cache = LlmsTxtCache::new(source, meta());
        let snap = cache.build().await.unwrap();
        let body = &snap.pages[0];
        assert!(body.contains("[a title]"));
        assert!(body.contains("[b title]"));
        assert!(body.contains("[c title]"));
    }

    #[tokio::test]
    async fn cache_get_or_build_caches_snapshot() {
        let source: DatasetSource = Arc::new(StubSearcher::with_pages(vec![SearchPage {
            hits: vec![fixture_hit("a", None)],
            next_offset: None,
        }]));
        let cache = LlmsTxtCache::new(source, meta());
        let first = cache.get_or_build().await.unwrap();
        // Second call must return the cached snapshot — the stub
        // has exhausted its responses, so a rebuild would yield an
        // empty page and the assertion would fail.
        let second = cache.get_or_build().await.unwrap();
        assert_eq!(first.etag, second.etag);
        assert!(second.pages[0].contains("[a title]"));
    }

    #[tokio::test]
    async fn invalidate_forces_rebuild_on_next_request() {
        let source: DatasetSource = Arc::new(StubSearcher::with_pages(vec![
            SearchPage {
                hits: vec![fixture_hit("first", None)],
                next_offset: None,
            },
            SearchPage {
                hits: vec![fixture_hit("second", None)],
                next_offset: None,
            },
        ]));
        let cache = LlmsTxtCache::new(source, meta());
        let first = cache.get_or_build().await.unwrap();
        assert!(first.pages[0].contains("[first title]"));
        cache.invalidate().await;
        let second = cache.get_or_build().await.unwrap();
        assert!(second.pages[0].contains("[second title]"));
        assert_ne!(first.etag, second.etag);
    }

    #[test]
    fn single_page_index_points_back_at_llms_txt() {
        let body = render_single_page_index(&meta());
        assert!(body.contains("https://example.test/llms.txt"));
    }

    #[test]
    fn paginate_emits_cross_links_per_page() {
        let limits = small_limits();
        let mut hits = Vec::new();
        for i in 0..30 {
            hits.push(fixture_hit(
                &format!("ds-{i:04}"),
                Some("sensor reading description that survives truncation"),
            ));
        }
        let pages = paginate_hits(&meta(), &hits, &limits);
        assert!(pages.len() >= 2);
        // First page references the next, not the previous.
        assert!(!pages[0].contains("Previous page"));
        assert!(pages[0].contains("Next page: <https://example.test/llms-page-2.txt>"));
        // Last page still emits a forward link; 404 marks the end
        // is the documented contract.
        let last = pages.last().unwrap();
        assert!(last.contains("Catalog index: <https://example.test/llms-index.txt>"));
    }
}
