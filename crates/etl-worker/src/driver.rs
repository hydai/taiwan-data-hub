//! Crawl driver: walks a `SourceConnector` to completion and upserts
//! each `DatasetMetadata` into Postgres.
//!
//! Factored out of `main.rs` so it's testable without a tokio binary:
//! tests construct a wiremock-backed connector + a testcontainers
//! Postgres and call `run_one_pass` directly.

use std::collections::BTreeMap;

use connectors::{DatasetMetadata, Page, SourceConnector};
use sha2::{Digest, Sha256};
use storage::DatasetWriter;
use tools_data::domains;

/// How many per-dataset "no domain match" lines to log at WARN per
/// pass before we drop to DEBUG. 10 is enough for an operator to
/// eyeball the first batch of upstream categories that missed our
/// mapping table without flooding alerts when the misses are
/// numerous.
const SKIP_WARN_BUDGET: u32 = 10;

/// Outcome counters from a single crawl pass. Useful for ops dashboards
/// and tests; the binary logs these at the end of every run.
///
/// `skipped_no_domain` and `skipped_no_seed` are split because they map
/// to different operator responses: the former is a *content* gap
/// (extend `config/domains.yaml` or add a per-source mapping override
/// in #1.4d), the latter is a *deploy* gap (a migration didn't run, or
/// the YAML adds a slug that the migration hasn't seeded).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CrawlSummary {
    pub upserted: u64,
    pub skipped_no_domain: u64,
    pub skipped_no_seed: u64,
    /// Datasets whose metadata fingerprint changed (or were brand
    /// new) and got a fresh `dataset_versions` row. A pass that
    /// reaches steady state ought to land near zero — the system
    /// only writes versions when upstream actually changes.
    pub versions_recorded: u64,
    pub pages: u32,
}

/// Outcome of resolving one dataset's upstream categories against the
/// internal domain table. `Option<i16>` would conflate the two
/// no-route cases (no upstream match vs. mapped-but-seed-missing) and
/// force the caller to log a generic message that's misleading for the
/// seed-missing case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DomainResolution {
    Mapped(i16),
    NoMapping,
    SeedMissing,
}

/// Errors that bubble out of one crawl pass.
#[derive(Debug, thiserror::Error)]
pub enum CrawlError {
    #[error("connector error: {0}")]
    Connector(#[from] connectors::ConnectorError),
    #[error("storage error: {0}")]
    Storage(#[from] storage::StorageError),
}

/// Drive a single end-to-end crawl: drain the connector's pagination,
/// resolve each dataset's domain by upstream category, and upsert into
/// Postgres. Returns the summary for caller-side logging.
///
/// Datasets that don't map to any of the 20 internal domains are
/// **skipped** rather than dropped into a fallback bucket — better to
/// have a visible gap (`skipped_no_domain` count) than silently park
/// uncategorised data in the wrong place. #1.4d will add a per-source
/// mapping override config for the tail-of-the-tail cases.
pub async fn run_one_pass<C, W>(connector: &C, storage: &W) -> Result<CrawlSummary, CrawlError>
where
    C: SourceConnector,
    W: DatasetWriter,
{
    let source = connector.source_id();
    let mut summary = CrawlSummary::default();
    let mut cursor = None;
    // `Option<i16>` rather than `i16`: caches **negative** lookups too
    // (a domain slug not present in the `domains` table seed). Without
    // this, every dataset that maps to a missing-slug would re-issue
    // the SQL probe AND re-emit a WARN log line, which under a YAML /
    // migration mismatch could mean 50k+ extra queries per crawl.
    let mut domain_cache: BTreeMap<String, Option<i16>> = BTreeMap::new();
    // Bound the per-dataset skip-WARN volume: at most the first
    // SKIP_WARN_BUDGET get logged at WARN; the rest land at DEBUG so
    // ops alerting on WARN doesn't get drowned out under a large
    // crawl. The aggregate count surfaces in the summary INFO line.
    let mut skip_warn_remaining: u32 = SKIP_WARN_BUDGET;

    loop {
        // Increment AFTER a successful page fetch so the counter
        // reflects pages we actually completed. If `list_datasets`
        // errors, the `?` propagates and `pages` stays at the
        // last-good value — matters for the summary an operator
        // reads after a crash.
        //
        // `cursor.take()` moves the `Option<Cursor>` (which owns the
        // underlying `String`) into the call rather than cloning it
        // per page; `cursor` is unconditionally re-assigned below
        // from `next`, so the temporary `None` is irrelevant.
        let Page { items, next, total } = connector.list_datasets(cursor.take()).await?;
        summary.pages = summary.pages.saturating_add(1);
        // `?total` renders the `Option<u64>` via Debug — `None` /
        // `Some(123)` — without allocating a fallback `String` per
        // page even when debug logging is disabled. Matters for a
        // 50k-row crawl that paginates ~500 times.
        tracing::debug!(
            page = summary.pages,
            batch = items.len(),
            ?total,
            "fetched page"
        );

        for meta in items {
            match resolve_domain_id(storage, &meta, &mut domain_cache).await? {
                DomainResolution::Mapped(domain_id) => {
                    let dataset_id = storage.upsert_dataset(domain_id, source, &meta).await?;
                    summary.upserted = summary.upserted.saturating_add(1);

                    // Schema-diff recording (#1.4d). Compute a stable
                    // checksum over the metadata fields whose change
                    // we care about and derive a version label from
                    // it. The storage layer dedupes on
                    // `(dataset_id, version)` via `ON CONFLICT DO
                    // NOTHING`, so a no-op return means "this exact
                    // version has already been recorded for this
                    // dataset" — usually because the metadata is
                    // unchanged since last crawl, OR because upstream
                    // oscillated back to a state we've seen before.
                    // Steady-state crawls land near zero increments.
                    let checksum = metadata_checksum(&meta);
                    let version = version_string(&meta, &checksum);
                    match storage
                        .record_version_if_changed(dataset_id, &version, &checksum)
                        .await?
                    {
                        Some(_) => {
                            summary.versions_recorded = summary.versions_recorded.saturating_add(1);
                        }
                        None => {
                            tracing::trace!(
                                slug = %meta.slug,
                                "version already recorded for this dataset",
                            );
                        }
                    }
                }
                DomainResolution::NoMapping => {
                    // Only the FIRST WARN this pass carries the
                    // "budget exists" explanation; subsequent budgeted
                    // WARNs are concise. Otherwise WARN #2..N would
                    // each claim "further skips log at DEBUG" while
                    // there are still WARNs coming, which is false
                    // for every line except the last.
                    if skip_warn_remaining == SKIP_WARN_BUDGET {
                        tracing::warn!(
                            slug = %meta.slug,
                            categories = ?meta.upstream_categories,
                            skip_warn_budget = SKIP_WARN_BUDGET,
                            "no domain match — dataset skipped (WARNs bounded per pass; further skips beyond the budget log at DEBUG)",
                        );
                        skip_warn_remaining -= 1;
                    } else if skip_warn_remaining > 0 {
                        tracing::warn!(
                            slug = %meta.slug,
                            categories = ?meta.upstream_categories,
                            "no domain match — dataset skipped",
                        );
                        skip_warn_remaining -= 1;
                    } else {
                        tracing::debug!(
                            slug = %meta.slug,
                            categories = ?meta.upstream_categories,
                            "no domain match — dataset skipped",
                        );
                    }
                    summary.skipped_no_domain = summary.skipped_no_domain.saturating_add(1);
                }
                DomainResolution::SeedMissing => {
                    // `resolve_domain_id` already emitted the one-shot
                    // WARN naming the offending slug (and cached the
                    // miss so subsequent datasets routing to it don't
                    // re-warn). Per-dataset trace stays at DEBUG so a
                    // `RUST_LOG=debug` run can still surface the full
                    // affected list without flooding default logs.
                    tracing::debug!(
                        slug = %meta.slug,
                        "domain mapped but DB seed missing — dataset skipped",
                    );
                    summary.skipped_no_seed = summary.skipped_no_seed.saturating_add(1);
                }
            }
        }

        cursor = next;
        if cursor.is_none() {
            break;
        }
    }

    tracing::info!(
        source = %source,
        upserted = summary.upserted,
        skipped_no_domain = summary.skipped_no_domain,
        skipped_no_seed = summary.skipped_no_seed,
        versions_recorded = summary.versions_recorded,
        pages = summary.pages,
        "crawl pass complete"
    );
    Ok(summary)
}

/// Canonical fingerprint over the metadata fields that matter for
/// "did this dataset change since the last crawl?". We hash with
/// SHA-256 hex-encoded because:
///
/// - `std::hash::DefaultHasher` isn't stable across binary builds,
///   so a re-deploy would invalidate every stored checksum.
/// - The checksum survives in the DB and gets compared on every
///   crawl, so collision resistance matters; 256 bits is overkill
///   for the corpus size (~50k datasets) but the cost is trivial.
///
/// Field selection rationale:
/// - `source_id` — the (`source`, `source_id`) pair is the natural
///   key in the `datasets` table; including it in the checksum
///   pins identity but doesn't drive churn (it never changes for
///   a given dataset row).
/// - `slug` — CKAN's `name` can change while `source_id` stays put
///   (e.g. an agency renames the published dataset), and the slug
///   feeds marketplace URLs (`/data/<slug>`). Treating a slug rename
///   as version-worthy churn lets us record the URL-affecting edit.
/// - `title_i18n` + `description_i18n` + `publisher` —
///   user-visible churn.
/// - `license` + `update_frequency` + `original_url` — operational
///   churn that downstream tooling cares about.
/// - `last_modified_at` — upstream's own freshness signal.
///
/// We deliberately exclude `upstream_categories` because the ETL
/// already maps those to a `domain_id` via `map_to_domain`, and we
/// don't want a re-shuffled (but semantically equivalent) category
/// list to churn the version log.
fn metadata_checksum(meta: &DatasetMetadata) -> String {
    let mut hasher = Sha256::new();
    feed_field(&mut hasher, "source_id", &meta.source_id);
    feed_field(&mut hasher, "slug", &meta.slug);
    feed_i18n_field(&mut hasher, "title_i18n", &meta.title_i18n);
    feed_i18n_field(&mut hasher, "description_i18n", &meta.description_i18n);
    feed_field(&mut hasher, "license", &meta.license);
    feed_field(
        &mut hasher,
        "publisher",
        meta.publisher.as_deref().unwrap_or(""),
    );
    feed_field(
        &mut hasher,
        "update_frequency",
        meta.update_frequency.as_deref().unwrap_or(""),
    );
    feed_field(
        &mut hasher,
        "original_url",
        meta.original_url.as_deref().unwrap_or(""),
    );
    feed_field(
        &mut hasher,
        "last_modified_at",
        &meta
            .last_modified_at
            .map(|d| d.to_rfc3339())
            .unwrap_or_default(),
    );
    let digest = hasher.finalize();
    format!("sha256:{digest:x}")
}

/// Append `<label>\0<value>\0` to the hasher. The NUL separators
/// prevent label/value collisions (e.g. `slug=ab` + `name=c` vs
/// `slug=a` + `name=bc`).
fn feed_field(hasher: &mut Sha256, label: &str, value: &str) {
    hasher.update(label.as_bytes());
    hasher.update([0]);
    hasher.update(value.as_bytes());
    hasher.update([0]);
}

/// Append a `BTreeMap` (sorted-by-key) to the hasher under a label.
/// `BTreeMap`'s iteration order is deterministic which gives us
/// stable checksums across runs.
fn feed_i18n_field(hasher: &mut Sha256, label: &str, map: &BTreeMap<String, String>) {
    hasher.update(label.as_bytes());
    hasher.update([0]);
    for (locale, text) in map {
        hasher.update(locale.as_bytes());
        hasher.update([0]);
        hasher.update(text.as_bytes());
        hasher.update([0]);
    }
}

/// Human-comparable version label for a `dataset_versions` row.
///
/// Appends the **full** SHA-256 hex digest as a suffix so the
/// version label is injectively derived from the checksum — two
/// distinct checksums produce two distinct version labels, with
/// no probabilistic gap to worry about. The
/// `record_version_if_changed` storage call relies on this
/// injectivity for its `ON CONFLICT (dataset_id, version) DO NOTHING`
/// shape: a checksum change always surfaces as a new version label,
/// so the conflict path only fires when we've genuinely seen this
/// exact checksum for this exact dataset before.
///
/// Shapes (note the `sha256:` prefix is part of the checksum string
/// emitted by [`metadata_checksum`] and is preserved here):
/// - `last_modified_at` present: `"<rfc3339-ts>#sha256:<64-hex>"` —
///   timestamp first so operators recognise the era at a glance;
///   `#`-separated full hash makes the label content-addressable.
/// - missing: `"sha256:<64-hex>"` (no good human-readable era, so
///   the hash leads).
fn version_string(meta: &DatasetMetadata, checksum: &str) -> String {
    match meta.last_modified_at {
        Some(ts) => format!("{}#{checksum}", ts.to_rfc3339()),
        None => checksum.to_owned(),
    }
}

/// Resolve `meta.upstream_categories` → `domain_id` via
/// [`tools_data::domains::map_to_domain`], then look up the surrogate
/// id in Postgres. Caches both **positive** lookups (so a 50k-row
/// crawl issues at most 20 queries) and **negative** ones (so a
/// missing-from-DB slug doesn't re-query + re-warn for every dataset
/// that maps to it).
///
/// Returns a [`DomainResolution`] so the caller can distinguish "no
/// upstream category matched any domain" from "a domain matched, but
/// its slug isn't seeded in the DB" — the two cases need different
/// log messages and different counters.
async fn resolve_domain_id<W: DatasetWriter>(
    storage: &W,
    meta: &DatasetMetadata,
    cache: &mut BTreeMap<String, Option<i16>>,
) -> Result<DomainResolution, CrawlError> {
    let Some(domain) = domains::map_to_domain(&meta.upstream_categories) else {
        return Ok(DomainResolution::NoMapping);
    };
    if let Some(cached) = cache.get(&domain.slug) {
        return Ok(match cached {
            Some(id) => DomainResolution::Mapped(*id),
            None => DomainResolution::SeedMissing,
        });
    }
    let resolved = storage.domain_id_for_slug(&domain.slug).await?;
    if resolved.is_none() {
        // Mapping returned a slug not present in the DB — very unusual
        // (domains are seeded at migration time) but plausible if a
        // future YAML revision is missing its migration. WARN once,
        // cache the miss, then treat all subsequent datasets that
        // resolve to this slug as `SeedMissing` silently.
        tracing::warn!(
            slug = %domain.slug,
            "domain seed not in DB; mapping skipped (further misses cached, won't re-warn)",
        );
    }
    cache.insert(domain.slug.clone(), resolved);
    Ok(match resolved {
        Some(id) => DomainResolution::Mapped(id),
        None => DomainResolution::SeedMissing,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::sync::Mutex;

    use super::*;
    use async_trait::async_trait;
    use connectors::data_gov_tw::DataGovTwConnector;
    use connectors::{ConnectorError, Cursor, SourceId};
    use serde_json::json;
    use storage::{Storage, StorageError};
    use testcontainers_modules::postgres::Postgres as PgContainer;
    use testcontainers_modules::testcontainers::ContainerAsync;
    use testcontainers_modules::testcontainers::ImageExt;
    use testcontainers_modules::testcontainers::runners::AsyncRunner;
    use uuid::Uuid;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Docker-free `SourceConnector` that yields a pre-baked queue of
    /// pages and panics if the driver requests more than were set up.
    struct StubConnector {
        pages: Mutex<VecDeque<Page>>,
    }

    #[async_trait]
    impl SourceConnector for StubConnector {
        fn source_id(&self) -> SourceId {
            SourceId::DataGovTw
        }
        async fn list_datasets(&self, _cursor: Option<Cursor>) -> Result<Page, ConnectorError> {
            Ok(self
                .pages
                .lock()
                .unwrap()
                .pop_front()
                .expect("stub connector exhausted — test asked for more pages than were set up"))
        }
    }

    /// In-memory `DatasetWriter`. `domains` is the slug→id seed; an
    /// upstream category that maps via `tools_data::domains` to a slug
    /// **not** in this table exercises the `SeedMissing` branch.
    ///
    /// Mirrors the real storage's **natural-key dedup** for
    /// `record_version_if_changed`: tracks the set of
    /// `(dataset_id, version)` pairs seen so far, and the second
    /// call with the same pair returns `None` — same shape as
    /// Postgres's `ON CONFLICT (dataset_id, version) DO NOTHING`.
    /// Without this fidelity, unit tests would happily accept calls
    /// that real Postgres would reject (e.g. an A → B → A
    /// oscillation appearing to insert three rows in the stub but
    /// only two in production).
    struct StubStorage {
        domains: HashMap<String, i16>,
        upserts: Mutex<Vec<(i16, String)>>,
        /// slug → dataset uuid
        dataset_ids: Mutex<HashMap<String, Uuid>>,
        /// Versions we've seen — mirrors UNIQUE (`dataset_id`, version).
        seen_versions: Mutex<HashSet<(Uuid, String)>>,
    }

    #[async_trait]
    impl DatasetWriter for StubStorage {
        async fn upsert_dataset(
            &self,
            domain_id: i16,
            _source: SourceId,
            metadata: &DatasetMetadata,
        ) -> Result<Uuid, StorageError> {
            self.upserts
                .lock()
                .unwrap()
                .push((domain_id, metadata.slug.clone()));
            // Assign a stable per-slug Uuid on first upsert so
            // record_version_if_changed sees a consistent identity.
            let mut ids = self.dataset_ids.lock().unwrap();
            let id = *ids
                .entry(metadata.slug.clone())
                .or_insert_with(Uuid::new_v4);
            Ok(id)
        }
        async fn domain_id_for_slug(&self, slug: &str) -> Result<Option<i16>, StorageError> {
            Ok(self.domains.get(slug).copied())
        }
        async fn record_version_if_changed(
            &self,
            dataset_id: Uuid,
            version: &str,
            _checksum: &str,
        ) -> Result<Option<Uuid>, StorageError> {
            let mut seen = self.seen_versions.lock().unwrap();
            if seen.insert((dataset_id, version.to_owned())) {
                Ok(Some(Uuid::new_v4()))
            } else {
                Ok(None)
            }
        }
    }

    /// Checksum must be stable for identical metadata across calls
    /// and across binary builds (we use SHA-256 explicitly, not
    /// `DefaultHasher`, for exactly this reason).
    #[test]
    fn metadata_checksum_is_stable_and_field_sensitive() {
        let base = fixture_meta("air-quality", "環境");
        let twin = fixture_meta("air-quality", "環境");
        assert_eq!(
            metadata_checksum(&base),
            metadata_checksum(&twin),
            "identical metadata must hash the same",
        );

        // A change in any tracked field must flip the checksum.
        let mut license_changed = base.clone();
        license_changed.license = "CC-BY-4.0".into();
        assert_ne!(
            metadata_checksum(&base),
            metadata_checksum(&license_changed),
        );

        let mut title_changed = base.clone();
        title_changed
            .title_i18n
            .insert("zh-TW".into(), "different title".into());
        assert_ne!(metadata_checksum(&base), metadata_checksum(&title_changed),);

        // upstream_categories is deliberately NOT in the checksum
        // because the ETL already maps it to a domain_id; a
        // reshuffled-but-equivalent category list should not churn
        // the version log.
        let mut categories_changed = base.clone();
        categories_changed
            .upstream_categories
            .push("不同分類".into());
        assert_eq!(
            metadata_checksum(&base),
            metadata_checksum(&categories_changed),
            "upstream_categories should not influence the checksum",
        );
    }

    #[test]
    fn version_string_embeds_full_checksum() {
        let no_ts = fixture_meta("x", "y");
        assert!(no_ts.last_modified_at.is_none());
        let fallback = version_string(&no_ts, "sha256:abcdef0123456789");
        assert_eq!(
            fallback, "sha256:abcdef0123456789",
            "no-ts shape is just the checksum",
        );

        let mut with_ts = no_ts.clone();
        with_ts.last_modified_at = Some(
            chrono::DateTime::parse_from_rfc3339("2026-04-15T03:30:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let from_ts = version_string(&with_ts, "sha256:abcdef0123456789");
        assert_eq!(
            from_ts, "2026-04-15T03:30:00+00:00#sha256:abcdef0123456789",
            "with-ts shape is `<ts>#<checksum>`",
        );
    }

    /// Critical concurrency-safety contract: two different checksums
    /// with the SAME upstream timestamp must produce different version
    /// labels. The storage layer's `ON CONFLICT (dataset_id, version)
    /// DO NOTHING` relies on this — without it a publisher-only edit
    /// (timestamp unchanged) would silently lose its version row.
    #[test]
    fn version_string_disambiguates_distinct_checksums_at_same_timestamp() {
        let mut meta = fixture_meta("x", "y");
        meta.last_modified_at = Some(
            chrono::DateTime::parse_from_rfc3339("2026-04-15T03:30:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let real_a = metadata_checksum(&meta);
        let mut meta_b = meta.clone();
        meta_b.license = "different".into();
        let real_b = metadata_checksum(&meta_b);
        assert_ne!(real_a, real_b, "fixture sanity");
        let v_a = version_string(&meta, &real_a);
        let v_b = version_string(&meta, &real_b);
        assert_ne!(v_a, v_b, "same ts + different checksum must differ");
    }

    fn fixture_meta(slug: &str, category: &str) -> DatasetMetadata {
        DatasetMetadata {
            source_id: format!("{slug}-upstream-id"),
            slug: slug.to_owned(),
            title_i18n: BTreeMap::from([("zh-TW".to_owned(), format!("{slug} title"))]),
            description_i18n: BTreeMap::new(),
            license: "Open Government Data License".to_owned(),
            publisher: None,
            update_frequency: None,
            original_url: None,
            last_modified_at: None,
            upstream_categories: vec![category.to_owned()],
        }
    }

    /// Docker-free coverage of `run_one_pass`: three datasets across
    /// two pages exercise all three `DomainResolution` branches in one
    /// shot, so the full counter contract is locked into CI.
    #[tokio::test]
    async fn run_one_pass_classifies_mapped_seed_missing_and_no_mapping() {
        let connector = StubConnector {
            pages: Mutex::new(VecDeque::from([
                Page {
                    items: vec![
                        // Maps to "environment", which IS in stub domains → Mapped.
                        fixture_meta("air-quality", "環境"),
                        // Maps to "education-research", NOT in stub domains → SeedMissing.
                        fixture_meta("school-roster", "教育與研究"),
                    ],
                    next: Some(Cursor::new("2:2")),
                    total: Some(3),
                },
                Page {
                    items: vec![
                        // No upstream category matches any domain → NoMapping.
                        fixture_meta("mystery", "Something Off-Taxonomy"),
                    ],
                    next: None,
                    total: Some(3),
                },
            ])),
        };
        let storage = StubStorage {
            domains: HashMap::from([("environment".to_owned(), 7_i16)]),
            upserts: Mutex::new(Vec::new()),
            dataset_ids: Mutex::new(HashMap::new()),
            seen_versions: Mutex::new(HashSet::new()),
        };

        let summary = run_one_pass(&connector, &storage).await.expect("crawl ok");

        assert_eq!(summary.upserted, 1, "only environment is seeded");
        assert_eq!(summary.skipped_no_seed, 1, "education-research is unseeded");
        assert_eq!(summary.skipped_no_domain, 1, "mystery has no mapping");
        assert_eq!(
            summary.versions_recorded, 1,
            "fresh dataset gets one version row",
        );
        assert_eq!(summary.pages, 2);

        let upserts = storage.upserts.lock().unwrap();
        assert_eq!(upserts.len(), 1, "exactly one upsert");
        assert_eq!(
            upserts[0],
            (7, "air-quality".to_owned()),
            "upsert lands under the seeded domain id",
        );
    }

    /// Re-running the same crawl pass should NOT produce additional
    /// version rows — the schema-diff check kicks in on the second
    /// `record_version_if_changed` call and observes the matching
    /// checksum from the first pass.
    #[tokio::test]
    async fn run_one_pass_does_not_re_record_unchanged_versions() {
        let connector_pages = vec![Page {
            items: vec![fixture_meta("air-quality", "環境")],
            next: None,
            total: Some(1),
        }];
        let storage = StubStorage {
            domains: HashMap::from([("environment".to_owned(), 7_i16)]),
            upserts: Mutex::new(Vec::new()),
            dataset_ids: Mutex::new(HashMap::new()),
            seen_versions: Mutex::new(HashSet::new()),
        };

        // First pass — version row recorded.
        let connector = StubConnector {
            pages: Mutex::new(VecDeque::from(connector_pages.clone())),
        };
        let summary = run_one_pass(&connector, &storage).await.expect("pass 1");
        assert_eq!(summary.versions_recorded, 1);

        // Second pass with identical metadata — checksum matches,
        // no new version row.
        let connector = StubConnector {
            pages: Mutex::new(VecDeque::from(connector_pages)),
        };
        let summary = run_one_pass(&connector, &storage).await.expect("pass 2");
        assert_eq!(
            summary.versions_recorded, 0,
            "identical metadata must not insert a new version row",
        );
    }

    async fn fresh_storage() -> (Storage, ContainerAsync<PgContainer>) {
        let container = PgContainer::default()
            .with_tag("18-alpine")
            .start()
            .await
            .expect("start postgres container");
        let host = container.get_host().await.expect("host");
        let port = container.get_host_port_ipv4(5432).await.expect("port");
        let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");

        let pool = sqlx::PgPool::connect(&url).await.expect("connect");
        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .expect("migrate");
        (Storage::from_pool(pool), container)
    }

    fn ckan_dataset(id: &str, name: &str, group_title: &str) -> serde_json::Value {
        json!({
            "id": id,
            "name": name,
            "title": format!("{name} title"),
            "notes": format!("{name} notes"),
            "license_title": "Open Government Data License",
            "organization": {"name": "moi", "title": "Test Organization"},
            "groups": [{"name": "g1", "title": group_title}],
            "frequency": "monthly",
            "metadata_modified": "2026-04-15T03:30:00"
        })
    }

    /// End-to-end happy path: wiremock CKAN serves 3 datasets across 2
    /// pages, the driver upserts all three (one per domain) into a
    /// real Postgres 18 container, and the final counts match.
    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p etl-worker -- --ignored`"]
    async fn run_one_pass_drains_pagination_and_upserts() {
        let upstream = MockServer::start().await;

        // Page 0: two datasets, one mappable to realestate-land, the
        // other to environment.
        Mock::given(method("GET"))
            .and(path("/api/v2/rest/dataset"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "result": {
                    "count": 4,
                    "results": [
                        ckan_dataset("1", "land-prices", "不動產與土地"),
                        ckan_dataset("2", "air-quality", "環境"),
                    ]
                }
            })))
            .mount(&upstream)
            .await;

        // Page 1: one more, plus a deliberately-uncategorised dataset
        // that should be skipped.
        Mock::given(method("GET"))
            .and(path("/api/v2/rest/dataset"))
            .and(query_param("offset", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "result": {
                    "count": 4,
                    "results": [
                        ckan_dataset("3", "school-roster", "教育與研究"),
                        ckan_dataset("4", "mystery-meat", "Something Off-Taxonomy"),
                    ]
                }
            })))
            .mount(&upstream)
            .await;

        let connector = DataGovTwConnector::builder()
            .base_url(upstream.uri())
            .page_size(2)
            .build()
            .expect("build connector");

        let (storage, _container) = fresh_storage().await;
        let summary = run_one_pass(&connector, &storage).await.expect("crawl ok");

        assert_eq!(summary.upserted, 3, "three mappable datasets land");
        assert_eq!(summary.skipped_no_domain, 1, "off-taxonomy entry skipped");
        assert_eq!(summary.pages, 2);

        // Confirm rows landed under the expected domains.
        let by_slug: Vec<(String, String)> = sqlx::query_as(
            "SELECT d.slug, dom.slug FROM datasets d \
             JOIN domains dom ON dom.id = d.domain_id \
             ORDER BY d.slug",
        )
        .fetch_all(storage.pool())
        .await
        .expect("query");
        assert_eq!(
            by_slug,
            vec![
                ("air-quality".to_owned(), "environment".to_owned()),
                ("land-prices".to_owned(), "realestate-land".to_owned()),
                ("school-roster".to_owned(), "education-research".to_owned()),
            ]
        );
    }

    /// Empty-page termination + zero upserts.
    #[tokio::test]
    #[ignore = "requires docker; run with `cargo test -p etl-worker -- --ignored`"]
    async fn run_one_pass_handles_empty_catalog() {
        let upstream = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/rest/dataset"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "result": {"count": 0, "results": []}
            })))
            .mount(&upstream)
            .await;

        let connector = DataGovTwConnector::builder()
            .base_url(upstream.uri())
            .page_size(2)
            .build()
            .unwrap();
        let (storage, _container) = fresh_storage().await;
        let summary = run_one_pass(&connector, &storage).await.unwrap();
        assert_eq!(summary.upserted, 0);
        assert_eq!(summary.skipped_no_domain, 0);
    }
}
