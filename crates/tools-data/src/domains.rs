//! Seed data for the 20 dataset domains, parsed from
//! `config/domains.yaml`.
//!
//! Layered defenses against a malformed seed:
//!
//! 1. **`cargo test` (CI gate):** the unit test below parses the
//!    embedded YAML so a bad seed fails the Rust gate in CI before
//!    a binary ever ships.
//! 2. **Process boot:** `tools_data::register_data_tools` warms the
//!    `OnceLock` so any parse failure panics at startup, not on the
//!    first tool call. A reckless build that skipped tests still
//!    fails loudly before serving traffic.
//! 3. **Defense in depth:** lookups continue to go through
//!    `embedded()`, which panics on the (unreachable in practice)
//!    case where the cache was never initialised.
//!
//! `include_str!` is byte-level only — it doesn't know the file is
//! YAML — so there is no compile-time syntactic check without
//! adding a `build.rs`. For a single config file we lean on the
//! three layers above instead.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// Embedded YAML source. The relative path resolves at compile time
/// from `crates/tools-data/src/domains.rs` to the repo-root config.
const DOMAINS_YAML: &str = include_str!("../../../config/domains.yaml");

/// One domain entry, as authored in `config/domains.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Domain {
    pub slug: String,
    pub kind: DomainKind,
    pub sort_order: i32,
    pub name: I18nText,
    #[serde(default)]
    pub description: Option<I18nText>,
}

/// Top-level section a domain belongs to in the marketplace.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DomainKind {
    Topical,
    Meta,
    Horizontal,
}

impl DomainKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Topical => "topical",
            Self::Meta => "meta",
            Self::Horizontal => "horizontal",
        }
    }
}

/// Localizable text with `zh-TW` as the always-present source language and
/// optional additional locales (e.g. `en`).
///
/// `serde(flatten)` over a `BTreeMap` means *any* extra key becomes a locale
/// candidate. Unknown locales fall back to `zh-TW` per CLAUDE.md.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct I18nText {
    #[serde(rename = "zh-TW")]
    pub zh_tw: String,
    #[serde(flatten)]
    pub other: BTreeMap<String, String>,
}

impl I18nText {
    /// Pick the best string for `locale`. `zh-TW` is the source; any other
    /// locale either hits in `other` or falls back to `zh-TW`.
    pub fn resolve(&self, locale: &str) -> &str {
        if locale == "zh-TW" {
            return &self.zh_tw;
        }
        self.other
            .get(locale)
            .map_or(self.zh_tw.as_str(), String::as_str)
    }
}

/// Parse a YAML domain list. Public so tests can exercise the parser on
/// hand-crafted fixtures without rebuilding the binary.
pub fn parse(yaml: &str) -> Result<Vec<Domain>, serde_yml::Error> {
    serde_yml::from_str(yaml)
}

/// Map upstream category strings (CKAN groups, dataset tags, …) to
/// the best-fit internal [`Domain`] (whose `.slug` is the stable
/// identifier the rest of the system uses). Best-effort substring
/// match in either direction against each domain's zh-TW or English
/// name, run in `sort_order` so a tied match prefers the more
/// general bucket (`realestate-land` before `economy-business`, etc.).
///
/// Returns `None` when no upstream category contains — or is
/// contained by — any domain name. The ETL caller decides the
/// fallback (skip the dataset, log a warning, drop in a "misc"
/// bucket); the mapper itself stays opinion-free.
pub fn map_to_domain<S: AsRef<str>>(upstream: &[S]) -> Option<&'static Domain> {
    map_in(upstream, embedded())
}

/// Inner implementation, parameterised over the domain table so unit
/// tests can fixture pathological inputs (empty locale values, etc.)
/// without going through `embedded()`'s singleton cache.
fn map_in<'a, S: AsRef<str>>(upstream: &[S], domains: &'a [Domain]) -> Option<&'a Domain> {
    if upstream.is_empty() {
        return None;
    }
    for d in domains {
        let candidates = [d.name.zh_tw.as_str()]
            .into_iter()
            .chain(d.name.other.values().map(String::as_str));
        for cand in candidates {
            // Trim + empty-skip on BOTH sides. Without the candidate
            // guard, a future YAML revision shipping `en: ""` (which
            // the `every_domain_has_zh_tw_name` test wouldn't catch)
            // would make `raw.contains("")` trivially true for every
            // upstream string and silently route every dataset to the
            // first domain in `sort_order`.
            let cand = cand.trim();
            if cand.is_empty() {
                continue;
            }
            for raw in upstream {
                // Trim BEFORE the emptiness check so whitespace-only
                // inputs (e.g. CKAN sometimes emits `" "` for a missing
                // group title) don't slip through and substring-match
                // any candidate that happens to contain a space — that
                // would silently land such datasets under whatever
                // domain comes first in `sort_order`.
                let raw = raw.as_ref().trim();
                if raw.is_empty() {
                    continue;
                }
                if cand.contains(raw) || raw.contains(cand) {
                    return Some(d);
                }
            }
        }
    }
    None
}

/// Cached view of the embedded `config/domains.yaml`, **sorted by
/// `sort_order`** so iteration semantics don't depend on the order
/// the maintainer happens to have written the YAML in. Tools like
/// `list_domains` and `map_to_domain` rely on this invariant.
///
/// Panics on first use if the YAML is malformed — which would mean
/// the binary was built from a broken `config/domains.yaml`. The
/// unit test below guarantees this doesn't ship.
pub fn embedded() -> &'static [Domain] {
    static CACHE: OnceLock<Vec<Domain>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let mut domains = parse(DOMAINS_YAML).expect("config/domains.yaml must parse");
            // Stable sort so equal `sort_order` rows preserve their
            // authored order — relevant for the kind grouping
            // (topical/meta/horizontal) where each kind has a
            // contiguous sort_order range.
            domains.sort_by_key(|d| d.sort_order);
            domains
        })
        .as_slice()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_yaml_parses_and_has_twenty_entries() {
        let domains = embedded();
        assert_eq!(
            domains.len(),
            20,
            "config/domains.yaml must seed 20 domains"
        );
    }

    #[test]
    fn kinds_are_recognised() {
        let kinds: Vec<DomainKind> = embedded().iter().map(|d| d.kind).collect();
        let topical = kinds
            .iter()
            .filter(|k| matches!(k, DomainKind::Topical))
            .count();
        let meta = kinds
            .iter()
            .filter(|k| matches!(k, DomainKind::Meta))
            .count();
        let horizontal = kinds
            .iter()
            .filter(|k| matches!(k, DomainKind::Horizontal))
            .count();
        // Per DESIGN.md §1.2: 17 topical + 1 meta + 2 horizontal = 20.
        assert_eq!((topical, meta, horizontal), (17, 1, 2));
    }

    #[test]
    fn every_domain_has_zh_tw_name() {
        for d in embedded() {
            assert!(!d.name.zh_tw.is_empty(), "{} missing zh-TW name", d.slug);
        }
    }

    #[test]
    fn i18n_resolve_falls_back_to_zh_tw_for_unknown_locale() {
        let text = I18nText {
            zh_tw: "中文".into(),
            other: BTreeMap::from([("en".to_owned(), "english".to_owned())]),
        };
        assert_eq!(text.resolve("zh-TW"), "中文");
        assert_eq!(text.resolve("en"), "english");
        assert_eq!(text.resolve("ja"), "中文");
        assert_eq!(text.resolve(""), "中文");
    }

    #[test]
    fn map_to_domain_matches_zh_tw_substring() {
        // CKAN's group title equals the domain's zh-TW name exactly.
        let d = map_to_domain(&["環境"]).expect("matches `environment`");
        assert_eq!(d.slug, "environment");
    }

    #[test]
    fn map_to_domain_matches_english_substring() {
        let d = map_to_domain(&["Real estate & land"]).expect("matches by en");
        assert_eq!(d.slug, "realestate-land");
    }

    #[test]
    fn map_to_domain_handles_partial_substrings_in_either_direction() {
        // Upstream tag is a prefix of the domain's en name → match.
        let d = map_to_domain(&["Real estate"]).expect("prefix matches");
        assert_eq!(d.slug, "realestate-land");
        // Upstream tag is a superstring of a domain name → also match.
        // Use the EXACT en form ("&", not "and") because the matcher
        // does literal substring containment, not synonym fuzzing.
        let d = map_to_domain(&["Education & research data archive"]).expect("superstring matches");
        assert_eq!(d.slug, "education-research");
    }

    #[test]
    fn map_to_domain_returns_none_for_empty_or_unknown_categories() {
        let none: [&str; 0] = [];
        assert!(map_to_domain(&none).is_none(), "empty input → None");
        assert!(map_to_domain(&[""]).is_none(), "empty string → None");
        assert!(
            map_to_domain(&["totally unrelated category nobody uses"]).is_none(),
            "non-matching category → None",
        );
    }

    #[test]
    fn map_to_domain_returns_none_for_whitespace_only_categories() {
        // Without the trim-before-emptiness-check fix, a lone space
        // would substring-match the first English candidate containing
        // a space (e.g. "Real estate & land") and silently misroute
        // the dataset.
        assert!(map_to_domain(&[" "]).is_none(), "single space → None");
        assert!(
            map_to_domain(&["\t\n  "]).is_none(),
            "tabs + newline + spaces → None",
        );
    }

    #[test]
    fn map_to_domain_trims_surrounding_whitespace_before_matching() {
        // Trimming makes the matcher resilient to upstream whitespace
        // hygiene issues (CKAN payloads often have trailing newlines
        // from XML→JSON conversion).
        let d = map_to_domain(&["  環境  "]).expect("trimmed zh-TW match");
        assert_eq!(d.slug, "environment");
        let d = map_to_domain(&["\tReal estate & land\n"]).expect("trimmed en match");
        assert_eq!(d.slug, "realestate-land");
    }

    /// Build a `Domain` for fixture use. Hand-rolled rather than via
    /// `parse()` so tests can express degenerate states (empty
    /// strings, missing locales) that the YAML wouldn't naturally
    /// produce.
    fn fixture(slug: &str, sort_order: i32, zh_tw: &str, en: Option<&str>) -> Domain {
        let mut other = BTreeMap::new();
        if let Some(en) = en {
            other.insert("en".to_owned(), en.to_owned());
        }
        Domain {
            slug: slug.to_owned(),
            kind: DomainKind::Topical,
            sort_order,
            name: I18nText {
                zh_tw: zh_tw.to_owned(),
                other,
            },
            description: None,
        }
    }

    #[test]
    fn map_skips_domains_with_empty_locale_value() {
        // First domain has `en: ""` (a plausible future YAML mistake).
        // Without the candidate-side empty guard, `"Second".contains("")`
        // returns true and the matcher would route the upstream tag to
        // the first domain in `sort_order`. With the guard, it falls
        // through to the second domain and matches its `en: "Second"`.
        let domains = [
            fixture("first", 1, "第一", Some("")),
            fixture("second", 2, "第二", Some("Second")),
        ];
        let d = map_in(&["Second"], &domains).expect("matches second");
        assert_eq!(d.slug, "second");
        // Also: an upstream that matches none of the *non-empty*
        // candidates must return None rather than falling into the
        // empty-string trap and emitting the first domain.
        assert!(map_in(&["nothing relevant"], &domains).is_none());
    }

    #[test]
    fn map_skips_domains_with_empty_or_whitespace_zh_tw() {
        // Symmetric defense for the zh-TW side: the embedded YAML test
        // (`every_domain_has_zh_tw_name`) already prevents this from
        // shipping, but the matcher belt-and-braces it.
        let domains = [
            fixture("blank", 1, "", None),
            fixture("blanks", 2, "   ", None),
            fixture("ok", 3, "OK", None),
        ];
        let d = map_in(&["OK"], &domains).expect("matches third");
        assert_eq!(d.slug, "ok");
        assert!(map_in(&["unrelated"], &domains).is_none());
    }

    #[test]
    fn slugs_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for d in embedded() {
            assert!(seen.insert(&d.slug), "duplicate slug: {}", d.slug);
        }
    }
}
