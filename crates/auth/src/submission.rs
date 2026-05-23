//! Submission service (#5a.1).
//!
//! Validates the per-kind payload, derives the moderation-list
//! `title` from typed fields, and writes a fresh `pending` row
//! through the [`SubmissionRepo`] trait. The trait split lets
//! the gateway handler depend on the service without touching
//! sqlx, and lets the tests run against an in-memory fake.
//!
//! Server-side validation is the canonical check. The
//! `SvelteKit` form runs the same rules client-side for UX
//! (instant feedback), but a tampered client cannot bypass
//! these — the gateway calls [`SubmissionService::create`]
//! before any DB round trip and rejects the request with
//! `AuthError::Validation` on a shape mismatch.

use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use storage::{NewSubmission, SubmissionKind, SubmissionRepo, SubmissionRow};
use uuid::Uuid;

use crate::error::AuthError;

/// Per-kind payload shape. The wire format is a single JSON
/// object with `kind` as the discriminator (`serde`'s
/// internally-tagged enum). Each variant carries exactly the
/// fields the moderator UI later needs — adding a field is a
/// service-layer edit, not a migration, because the row store
/// keeps the JSON opaque.
///
/// All variants share a common rule set:
///
///   * Every text field is trimmed before validation.
///   * Empty (post-trim) required fields are rejected.
///   * URLs are checked for an `http://` or `https://` scheme;
///     full RFC 3986 parsing happens in the upstream `url`
///     crate when ETL later consumes the row.
///   * Lengths are bounded so a single submission can't blow
///     past the 200-char `title` derivation or a hostile-actor
///     "novel-as-description" abuse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubmissionPayload {
    /// Community-contributed dataset entry. Once approved, an
    /// ETL pass (#5b.6 provenance) lifts it into the `datasets`
    /// table proper.
    Dataset {
        title: String,
        description: String,
        source_url: String,
        license: String,
        /// `domain_slug` is intentionally a free-text reference
        /// rather than a `domain_id` FK — the moderator
        /// reconciles it against the canonical `domains` table
        /// at approval time. Lets a contributor suggest a
        /// new domain too.
        domain_slug: String,
    },
    /// Community-contributed tool entry (e.g. a custom MCP
    /// tool the author wants to register).
    Tool {
        name: String,
        description: String,
        repo_url: String,
        /// `language` is the implementation language string
        /// ("rust" / "python" / "go" / …). The moderator
        /// reconciles it against a canonical taxonomy at
        /// approval; free text keeps the form simple.
        language: String,
    },
    /// Community-contributed connector entry (a new
    /// `SourceConnector` impl).
    Connector {
        name: String,
        description: String,
        repo_url: String,
        license: String,
    },
    /// Community-contributed playground entry (a demo or
    /// notebook).
    Playground {
        name: String,
        description: String,
        demo_url: String,
        /// Optional repo for the playground source. The
        /// moderator may require it on approval but the form
        /// allows quick prototypes without one.
        repo_url: Option<String>,
    },
}

impl SubmissionPayload {
    /// Wire-side kind, matching the storage enum so the row
    /// store records both copies consistently.
    #[must_use]
    pub const fn kind(&self) -> SubmissionKind {
        match self {
            Self::Dataset { .. } => SubmissionKind::Dataset,
            Self::Tool { .. } => SubmissionKind::Tool,
            Self::Connector { .. } => SubmissionKind::Connector,
            Self::Playground { .. } => SubmissionKind::Playground,
        }
    }
}

/// Maximum length of the materialised `submissions.title`
/// column. The form's title fields are bounded by
/// [`MAX_NAME_LEN`] (well under 200), so this acts as a final
/// safety clamp after derivation rather than a per-field cap.
pub const TITLE_MAX_LEN: usize = 200;

/// Maximum length of a name / title / license / slug input.
/// Keeps the moderator list view readable and prevents a
/// hostile actor from filling the JSONB with megabytes per row.
pub const MAX_NAME_LEN: usize = 120;

/// Maximum length of a free-text description. 2 KiB is plenty
/// for a submission's pitch — full README-style content is
/// expected to live behind the contributor's repo URL.
pub const MAX_DESCRIPTION_LEN: usize = 2048;

/// Maximum URL length. Matches the de-facto 2048-char limit
/// imposed by every major UA + a small headroom.
pub const MAX_URL_LEN: usize = 2048;

/// Composition root for the submission flow. Carries the
/// repository handle; thin enough to clone freely (the inner
/// `Arc` is the only field).
#[derive(Clone)]
pub struct SubmissionService {
    submissions: Arc<dyn SubmissionRepo>,
}

impl std::fmt::Debug for SubmissionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubmissionService").finish_non_exhaustive()
    }
}

impl SubmissionService {
    #[must_use]
    pub fn new(submissions: Arc<dyn SubmissionRepo>) -> Self {
        Self { submissions }
    }

    /// Validate the payload, derive a moderation-list title,
    /// and persist a `pending` row. Returns the assigned
    /// `submission_id` so the handler can echo it back to the
    /// `SvelteKit` form for the "view in my submissions" link.
    pub async fn create(
        &self,
        user_id: Uuid,
        payload: SubmissionPayload,
    ) -> Result<Uuid, AuthError> {
        let normalized = validate_and_normalize(payload)?;
        let title = derive_title(&normalized);
        let kind = normalized.kind();
        let json_payload = serde_json::to_value(&normalized).map_err(|e| {
            // `serde_json::to_value` only fails on cyclic /
            // non-string-keyed structures — neither is
            // representable in `SubmissionPayload`. Treating it
            // as an internal invariant violation rather than
            // a user-input error keeps the HTTP boundary at
            // 500 (where every other "should never happen"
            // case routes).
            AuthError::Internal(format!("submission payload serialization: {e}"))
        })?;
        let new = NewSubmission {
            user_id,
            kind,
            title,
            payload: ensure_kind_discriminator(json_payload, kind),
            created_at: Utc::now(),
        };
        let id = self.submissions.insert(new).await?;
        Ok(id)
    }

    /// "My submissions" list — every status, newest first.
    pub async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<SubmissionRow>, AuthError> {
        Ok(self.submissions.list_for_user(user_id).await?)
    }

    /// Author-side single-row fetch.
    pub async fn get_for_user(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<SubmissionRow>, AuthError> {
        Ok(self.submissions.get_for_user(id, user_id).await?)
    }

    /// Author-side withdraw. Returns `Ok(None)` for "not
    /// yours / not found / not pending" so the handler folds
    /// them into a single 404, matching the api-key revoke
    /// pattern.
    pub async fn withdraw(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<SubmissionRow>, AuthError> {
        Ok(self.submissions.withdraw(id, user_id, Utc::now()).await?)
    }
}

/// Run the per-kind validation rules and return a normalized
/// payload (all text fields trimmed). Errors are
/// [`AuthError::Validation`] so the gateway maps to `400`.
fn validate_and_normalize(payload: SubmissionPayload) -> Result<SubmissionPayload, AuthError> {
    match payload {
        SubmissionPayload::Dataset {
            title,
            description,
            source_url,
            license,
            domain_slug,
        } => {
            let title = require_text("title", &title, MAX_NAME_LEN)?;
            let description = require_text("description", &description, MAX_DESCRIPTION_LEN)?;
            let source_url = require_url("source_url", &source_url)?;
            let license = require_text("license", &license, MAX_NAME_LEN)?;
            let domain_slug = require_slug("domain_slug", &domain_slug)?;
            Ok(SubmissionPayload::Dataset {
                title,
                description,
                source_url,
                license,
                domain_slug,
            })
        }
        SubmissionPayload::Tool {
            name,
            description,
            repo_url,
            language,
        } => {
            let name = require_text("name", &name, MAX_NAME_LEN)?;
            let description = require_text("description", &description, MAX_DESCRIPTION_LEN)?;
            let repo_url = require_url("repo_url", &repo_url)?;
            let language = require_text("language", &language, MAX_NAME_LEN)?;
            Ok(SubmissionPayload::Tool {
                name,
                description,
                repo_url,
                language,
            })
        }
        SubmissionPayload::Connector {
            name,
            description,
            repo_url,
            license,
        } => {
            let name = require_text("name", &name, MAX_NAME_LEN)?;
            let description = require_text("description", &description, MAX_DESCRIPTION_LEN)?;
            let repo_url = require_url("repo_url", &repo_url)?;
            let license = require_text("license", &license, MAX_NAME_LEN)?;
            Ok(SubmissionPayload::Connector {
                name,
                description,
                repo_url,
                license,
            })
        }
        SubmissionPayload::Playground {
            name,
            description,
            demo_url,
            repo_url,
        } => {
            let name = require_text("name", &name, MAX_NAME_LEN)?;
            let description = require_text("description", &description, MAX_DESCRIPTION_LEN)?;
            let demo_url = require_url("demo_url", &demo_url)?;
            let repo_url = match repo_url {
                Some(v) => {
                    let trimmed = v.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(require_url("repo_url", trimmed)?)
                    }
                }
                None => None,
            };
            Ok(SubmissionPayload::Playground {
                name,
                description,
                demo_url,
                repo_url,
            })
        }
    }
}

fn require_text(field: &str, value: &str, max_len: usize) -> Result<String, AuthError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(AuthError::Validation(format!("{field}: required")));
    }
    if trimmed.chars().count() > max_len {
        return Err(AuthError::Validation(format!(
            "{field}: too long (max {max_len} characters)"
        )));
    }
    Ok(trimmed.to_owned())
}

fn require_url(field: &str, value: &str) -> Result<String, AuthError> {
    let trimmed = require_text(field, value, MAX_URL_LEN)?;
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err(AuthError::Validation(format!(
            "{field}: must start with http:// or https://"
        )));
    }
    Ok(trimmed)
}

fn require_slug(field: &str, value: &str) -> Result<String, AuthError> {
    let trimmed = require_text(field, value, MAX_NAME_LEN)?;
    // Permissive ASCII slug — letters / digits / `-` / `_` — so
    // the moderator can reconcile against the `domains.slug`
    // column at approval time without surprises. Unicode slugs
    // are intentionally rejected: the canonical `domains.slug`
    // is ASCII-only and accepting Unicode here would let two
    // visually-identical contributions slip past dedup.
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(AuthError::Validation(format!(
            "{field}: must be ASCII letters/digits/-/_"
        )));
    }
    Ok(trimmed)
}

/// Derive the `submissions.title` column from the typed
/// payload. Each kind has an obvious "primary name" field; we
/// truncate to [`TITLE_MAX_LEN`] characters as a final safety
/// clamp (per-field caps are already well below this).
fn derive_title(payload: &SubmissionPayload) -> String {
    let raw = match payload {
        SubmissionPayload::Dataset { title, .. } => title.as_str(),
        SubmissionPayload::Tool { name, .. }
        | SubmissionPayload::Connector { name, .. }
        | SubmissionPayload::Playground { name, .. } => name.as_str(),
    };
    if raw.chars().count() <= TITLE_MAX_LEN {
        raw.to_owned()
    } else {
        raw.chars().take(TITLE_MAX_LEN).collect()
    }
}

/// `serde_json::to_value` already emits the `kind` field on
/// our internally-tagged enum; this guard exists so a future
/// payload refactor cannot accidentally land a row without the
/// discriminator (the moderation queue parses on `kind` and a
/// missing field would silently mis-classify).
fn ensure_kind_discriminator(value: Value, kind: SubmissionKind) -> Value {
    let mut object = match value {
        Value::Object(o) => o,
        // Shape mismatch is unreachable in practice (the enum
        // is a struct-variant only). Coerce to a minimal
        // object so the row store keeps the invariant — and a
        // future-untyped reader still gets the kind.
        _ => serde_json::Map::new(),
    };
    object.insert("kind".to_owned(), json!(kind.as_str()));
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dataset_payload_round_trips_through_json() {
        let payload = SubmissionPayload::Dataset {
            title: "Taiwan rainfall observations".to_owned(),
            description: "Hourly rainfall observations from CWA stations.".to_owned(),
            source_url: "https://example.gov.tw/rainfall.csv".to_owned(),
            license: "CC-BY-4.0".to_owned(),
            domain_slug: "weather-climate".to_owned(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["kind"], "dataset");
        assert_eq!(json["title"], "Taiwan rainfall observations");
        let decoded: SubmissionPayload = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn validates_required_fields() {
        let err = validate_and_normalize(SubmissionPayload::Dataset {
            title: "   ".to_owned(),
            description: "ok".to_owned(),
            source_url: "https://example.com".to_owned(),
            license: "MIT".to_owned(),
            domain_slug: "ok".to_owned(),
        })
        .unwrap_err();
        match err {
            AuthError::Validation(msg) => assert!(msg.starts_with("title:")),
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn validates_url_scheme() {
        let err = validate_and_normalize(SubmissionPayload::Tool {
            name: "tdh-tool".to_owned(),
            description: "An MCP tool".to_owned(),
            repo_url: "ftp://example.com/x".to_owned(),
            language: "rust".to_owned(),
        })
        .unwrap_err();
        match err {
            AuthError::Validation(msg) => assert!(msg.contains("repo_url")),
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn validates_slug_charset() {
        let err = validate_and_normalize(SubmissionPayload::Dataset {
            title: "ok".to_owned(),
            description: "ok".to_owned(),
            source_url: "https://example.com/x.csv".to_owned(),
            license: "MIT".to_owned(),
            domain_slug: "天氣".to_owned(),
        })
        .unwrap_err();
        assert!(matches!(err, AuthError::Validation(_)));
    }

    #[test]
    fn truncates_title_to_max_len() {
        let long = "x".repeat(TITLE_MAX_LEN + 50);
        let payload = SubmissionPayload::Tool {
            name: long.clone(),
            description: "ok".to_owned(),
            repo_url: "https://example.com".to_owned(),
            language: "rust".to_owned(),
        };
        let title = derive_title(&payload);
        assert_eq!(title.chars().count(), TITLE_MAX_LEN);
    }

    #[test]
    fn playground_optional_repo_url() {
        let normalized = validate_and_normalize(SubmissionPayload::Playground {
            name: "weather-map".to_owned(),
            description: "Live rainfall heatmap".to_owned(),
            demo_url: "https://demo.example.com".to_owned(),
            repo_url: Some("  ".to_owned()),
        })
        .unwrap();
        if let SubmissionPayload::Playground { repo_url, .. } = normalized {
            assert!(repo_url.is_none(), "blank string should normalize to None");
        } else {
            panic!("expected playground kind");
        }
    }
}
