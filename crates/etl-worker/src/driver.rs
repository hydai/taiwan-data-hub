//! Crawl driver: walks a `SourceConnector` to completion and upserts
//! each `DatasetMetadata` into Postgres.
//!
//! Factored out of `main.rs` so it's testable without a tokio binary:
//! tests construct a wiremock-backed connector + a testcontainers
//! Postgres and call `run_one_pass` directly.

use std::collections::BTreeMap;

use connectors::{DatasetMetadata, Page, SourceConnector};
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
                    storage.upsert_dataset(domain_id, source, &meta).await?;
                    summary.upserted = summary.upserted.saturating_add(1);
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
        pages = summary.pages,
        "crawl pass complete"
    );
    Ok(summary)
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
    use std::collections::{HashMap, VecDeque};
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
    struct StubStorage {
        domains: HashMap<String, i16>,
        upserts: Mutex<Vec<(i16, String)>>,
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
            Ok(Uuid::nil())
        }
        async fn domain_id_for_slug(&self, slug: &str) -> Result<Option<i16>, StorageError> {
            Ok(self.domains.get(slug).copied())
        }
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
        };

        let summary = run_one_pass(&connector, &storage).await.expect("crawl ok");

        assert_eq!(summary.upserted, 1, "only environment is seeded");
        assert_eq!(summary.skipped_no_seed, 1, "education-research is unseeded");
        assert_eq!(summary.skipped_no_domain, 1, "mystery has no mapping");
        assert_eq!(summary.pages, 2);

        let upserts = storage.upserts.lock().unwrap();
        assert_eq!(upserts.len(), 1, "exactly one upsert");
        assert_eq!(
            upserts[0],
            (7, "air-quality".to_owned()),
            "upsert lands under the seeded domain id",
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
