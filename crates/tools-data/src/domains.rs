//! Seed data for the 20 dataset domains, parsed from
//! `config/domains.yaml` at build time.
//!
//! The YAML is embedded via `include_str!`, so binaries don't need
//! filesystem access at startup and a malformed YAML breaks `cargo
//! build` — not deployment. Lookup is cached in a `OnceLock` so the
//! YAML parse happens once per process.

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

/// Cached view of the embedded `config/domains.yaml`.
///
/// Panics on first use if the YAML is malformed — which would mean the
/// binary was built from a broken `config/domains.yaml`. The unit test
/// below guarantees this doesn't ship.
pub fn embedded() -> &'static [Domain] {
    static CACHE: OnceLock<Vec<Domain>> = OnceLock::new();
    CACHE
        .get_or_init(|| parse(DOMAINS_YAML).expect("config/domains.yaml must parse"))
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
    fn slugs_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for d in embedded() {
            assert!(seen.insert(&d.slug), "duplicate slug: {}", d.slug);
        }
    }
}
