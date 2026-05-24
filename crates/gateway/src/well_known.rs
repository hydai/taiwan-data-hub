//! `/.well-known/mcp.json` agent-discovery manifest (#7.2).
//!
//! Serves a static JSON document that advertises the gateway's MCP
//! presence to any client that follows the `/.well-known/mcp.json`
//! convention: server URL, transport, protocol version, auth posture,
//! license, and a flat summary of every registered tool. The body
//! is built once at boot from the live dispatcher + runtime config
//! and cached as [`Bytes`] so each request is a refcount bump.
//!
//! Distinct from the rmcp `tools/list` JSON-RPC method served at
//! `/mcp` — `tools/list` is the authoritative source for full
//! `input_schema` payloads (consumed by an MCP client mid-session);
//! `mcp.json` is a lightweight catalogue summary an agent can fetch
//! once to decide whether to wire up the server at all.

use std::fmt::Write as _;
use std::sync::Arc;

use axum::body::Bytes;
use axum::{
    Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use mcp_core::{Dispatcher, PROTOCOL_VERSION};
use serde::Serialize;
use sha2::{Digest, Sha256};
use shared::Mode;

/// Default `Content-Type` for well-known JSON bodies. RFC 8259
/// registers `application/json`; the `/.well-known/` path itself
/// follows RFC 8615's URI mechanism. Used for the MCP manifest,
/// the A2A agent card, the skills index, and the OAuth
/// protected-resource document (#7.4).
const JSON_CONTENT_TYPE: &str = "application/json; charset=utf-8";

/// `Content-Type` for the RFC 9727 API catalog. The spec
/// registers `application/linkset+json` as the media type for the
/// linkset document; intermediaries that key off Content-Type
/// (e.g. for content negotiation) need the specific value rather
/// than the generic `application/json`.
const LINKSET_CONTENT_TYPE: &str = "application/linkset+json";

/// `Cache-Control` value applied to every response. The manifest
/// only changes at gateway boot (tool registry is fixed for the
/// process lifetime, and base URL / mode are env-pinned), so a
/// long edge cache is safe. The strong `ETag` lets a fast-moving
/// CDN revalidate cheaply on a redeploy.
const CACHE_CONTROL: &str = "public, max-age=3600";

/// MCP protocol version the gateway speaks. Pulled from
/// [`mcp_core::PROTOCOL_VERSION`] so the manifest and the live
/// `/mcp` `initialize` response share a single source of truth —
/// an SDK upgrade that bumps the version surfaces as a test
/// failure in `mcp-core` rather than silent drift here.
const MCP_PROTOCOL_VERSION: &str = PROTOCOL_VERSION;

/// Streamable HTTP transport name per the MCP 2025-11-25 spec.
const MCP_TRANSPORT: &str = "streamable_http";

/// Runtime config the manifest needs that doesn't live on the
/// dispatcher. Built once at boot and consumed into the manifest
/// renderer.
#[derive(Debug, Clone)]
pub struct ManifestMeta {
    /// Public-facing base URL of the gateway (e.g. `https://hub.example`).
    /// The `/mcp` endpoint + auth resource pointer are derived from
    /// this — operators override via env so deployments behind a
    /// reverse proxy advertise the user-visible host, not the
    /// container's internal address.
    pub public_base_url: String,
    /// Display name advertised in the manifest's `name` field.
    /// Currently always the hard-coded default — an
    /// env-driven override (`MCP_SERVER_NAME`) is a follow-up
    /// once branded deployments need it.
    pub server_name: String,
    /// Tagline rendered as `description`. Same shape as
    /// `server_name`: default-only today, env-driven override
    /// when the contract calls for one.
    pub description: String,
    /// `Cargo.toml`-derived `CARGO_PKG_VERSION` of the gateway
    /// crate. Static `&'static str` because the value resolves at
    /// build time.
    pub server_version: &'static str,
    /// SPDX identifier for the licence the source code is published
    /// under. The workspace pin is `Apache-2.0` so the default is a
    /// literal; this field stays configurable for downstream forks.
    pub license: &'static str,
    /// Operating mode (`personal` vs `multi-user`). Drives the
    /// `auth` section: personal-mode advertises `none`, multi-user
    /// advertises `oauth2` plus a forward-reference to
    /// `/.well-known/oauth-protected-resource` (which #7.4 lands).
    pub mode: Mode,
    /// Provider organization name advertised in the Google A2A
    /// agent card (#7.3). Defaults to `Taiwan Data Hub Contributors`;
    /// downstream forks can override.
    pub provider_organization: String,
    /// Provider URL advertised in the agent card. Defaults to the
    /// upstream GitHub repo URL — operators almost certainly want
    /// to override for branded deployments.
    pub provider_url: String,
    /// Documentation URL advertised in the agent card. Defaults
    /// to the upstream repo README; downstream forks point at
    /// their own docs.
    pub documentation_url: String,
    /// OAuth authorization server URLs the gateway delegates to
    /// (#7.4). Per RFC 9728 the `authorization_servers` field is
    /// an array; empty by default and populated via the
    /// `OAUTH_AUTHORIZATION_SERVERS` env var (comma-separated)
    /// for multi-user deployments that front an external `IdP`.
    pub oauth_authorization_servers: Vec<String>,
}

impl ManifestMeta {
    /// Default values used when the gateway boots without
    /// `MCP_PUBLIC_URL` configured. Keeps the route working on a
    /// fresh laptop without any setup — the resolved URL is
    /// obviously a placeholder, but the JSON shape is valid and
    /// every field renders.
    pub fn defaults(mode: Mode, server_version: &'static str) -> Self {
        Self {
            public_base_url: "https://taiwan-data-hub.example".to_string(),
            server_name: "Taiwan Data Hub".to_string(),
            description: "Open Taiwan public data, exposed to AI agents via MCP.".to_string(),
            server_version,
            license: "Apache-2.0",
            mode,
            provider_organization: "Taiwan Data Hub Contributors".to_string(),
            provider_url: "https://github.com/hydai/taiwan-data-hub".to_string(),
            documentation_url: "https://github.com/hydai/taiwan-data-hub#readme".to_string(),
            // Default to no auth servers — personal-mode boots
            // and CI runs without OAUTH_AUTHORIZATION_SERVERS
            // get a spec-compliant empty array. RFC 9728 says
            // `authorization_servers` is OPTIONAL; emitting an
            // empty array is the documented shape for a
            // resource that doesn't delegate to any AS.
            oauth_authorization_servers: Vec::new(),
        }
    }
}

/// Pre-rendered manifest body together with its strong `ETag`
/// (the conditional-GET validator) and the `Content-Type` the
/// handler should set. Held behind `Arc` so every HTTP request
/// is a refcount bump rather than re-serialising the JSON.
///
/// `content_type` is a `&'static str` because every consumer
/// picks from one of the hand-curated module constants — a
/// runtime-built string would mean dynamic allocation per
/// response and an unnecessary `Cow<str>` shape.
#[derive(Debug, Clone)]
struct ManifestState {
    body: Bytes,
    etag: String,
    content_type: &'static str,
}

/// Build the well-known subrouter that mounts all three M7
/// agent-discovery surfaces (#7.2 + #7.3): `/.well-known/mcp.json`
/// (MCP manifest), `/.well-known/agent-card.json` (Google A2A
/// agent card), and `/.well-known/agent-skills.json` (skill →
/// MCP tool id index). Each body is rendered once from the
/// supplied [`Dispatcher`] snapshot + [`ManifestMeta`] at
/// construction time — the registry is fixed for the process
/// lifetime so a per-request rebuild would be wasted work.
pub fn router(dispatcher: &Dispatcher, meta: &ManifestMeta) -> Router {
    let mcp_state = build_state(&build_manifest(dispatcher, meta), JSON_CONTENT_TYPE);
    let card_state = build_state(&build_agent_card(dispatcher, meta), JSON_CONTENT_TYPE);
    let skills_state = build_state(&build_skills_index(dispatcher), JSON_CONTENT_TYPE);
    // #7.4 — RFC 9727 API catalog + RFC 9728 OAuth protected
    // resource. Both always-on: the catalog only references our
    // own endpoints, and the OAuth document is meaningful even in
    // personal mode (advertises an empty `authorization_servers`
    // array, which per RFC 9728 indicates a resource that doesn't
    // delegate to any auth server).
    let catalog_state = build_state(&build_api_catalog(meta), LINKSET_CONTENT_TYPE);
    let oauth_state = build_state(&build_oauth_resource(meta), JSON_CONTENT_TYPE);
    Router::new()
        .route(
            "/.well-known/mcp.json",
            get(generic_handler).with_state(mcp_state),
        )
        .route(
            "/.well-known/agent-card.json",
            get(generic_handler).with_state(card_state),
        )
        .route(
            "/.well-known/agent-skills.json",
            get(generic_handler).with_state(skills_state),
        )
        .route(
            "/.well-known/api-catalog",
            get(generic_handler).with_state(catalog_state),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(generic_handler).with_state(oauth_state),
        )
}

/// Pre-render a serialisable payload (MCP manifest, A2A agent
/// card, skills index, API catalog, or OAuth resource doc) into
/// the cached `body + etag + content-type` triple every handler
/// serves. Pulled out so all routes share the same allocation +
/// caching path.
fn build_state<T: Serialize>(payload: &T, content_type: &'static str) -> Arc<ManifestState> {
    // Every well-known payload is a statically-typed struct whose
    // field types are all `serde_json`-compatible — a panic here
    // would mean an upstream change introduced a non-serialisable
    // field, which is a programmer error, not a runtime input
    // problem.
    let body_json =
        serde_json::to_string_pretty(payload).expect("well-known payload serialises to JSON");
    let etag = etag_for(&body_json);
    Arc::new(ManifestState {
        body: Bytes::from(body_json),
        etag,
        content_type,
    })
}

async fn generic_handler(State(state): State<Arc<ManifestState>>, headers: HeaderMap) -> Response {
    serve_state(&state, &headers)
}

fn serve_state(state: &ManifestState, headers: &HeaderMap) -> Response {
    if let Some(resp) = not_modified_if_match(headers, &state.etag) {
        return resp;
    }
    success_response(state.body.clone(), &state.etag, state.content_type)
}

fn success_response(body: Bytes, etag: &str, content_type: &'static str) -> Response {
    let mut resp = (StatusCode::OK, body).into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL),
    );
    if let Ok(v) = HeaderValue::from_str(etag) {
        h.insert(header::ETAG, v);
    }
    resp
}

fn not_modified_if_match(headers: &HeaderMap, etag: &str) -> Option<Response> {
    let inm = headers.get(header::IF_NONE_MATCH)?.to_str().ok()?;
    if !if_none_match_matches(inm, etag) {
        return None;
    }
    let mut resp = StatusCode::NOT_MODIFIED.into_response();
    let h = resp.headers_mut();
    h.insert(header::ETAG, HeaderValue::from_str(etag).ok()?);
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL),
    );
    Some(resp)
}

/// RFC 9110 § 13.1.2-compliant `If-None-Match` comparison —
/// shares the contract documented in [`llms_txt`](crate::llms_txt).
/// Reimplemented here (rather than re-exporting) because the two
/// modules don't otherwise share state and a small parser is
/// cheaper than the cross-module surface a shared helper would
/// require.
fn if_none_match_matches(header_value: &str, etag: &str) -> bool {
    for entry in split_outside_quotes(header_value) {
        let entry = entry.trim();
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

/// Quote-aware comma split — commas inside `"..."` are preserved
/// because entity-tag opaque-tags can carry them literally.
fn split_outside_quotes(input: &str) -> Vec<&str> {
    let bytes = input.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut in_quotes = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate() {
        if escape {
            escape = false;
            continue;
        }
        match b {
            b'\\' if in_quotes => escape = true,
            b'"' => in_quotes = !in_quotes,
            b',' if !in_quotes => {
                parts.push(&input[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&input[start..]);
    parts
}

/// Strong `ETag` derived from the rendered body. 64-bit truncation
/// keeps the header short while leaving collision odds negligible
/// at the "one manifest per gateway boot" scale.
fn etag_for(body: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(18);
    hex.push('"');
    for byte in digest.iter().take(8) {
        let _ = write!(hex, "{byte:02x}");
    }
    hex.push('"');
    hex
}

/// Pure builder lifted out of [`router`] so unit tests can assert
/// on the manifest shape without spinning up an Axum runtime.
fn build_manifest(dispatcher: &Dispatcher, meta: &ManifestMeta) -> Manifest {
    let base = meta.public_base_url.trim_end_matches('/');
    let tools: Vec<ToolSummary> = dispatcher
        .list_tools()
        .into_iter()
        .map(|td| ToolSummary {
            name: td.name,
            description: td.description,
        })
        .collect();
    Manifest {
        name: meta.server_name.clone(),
        description: meta.description.clone(),
        version: meta.server_version.to_string(),
        license: meta.license.to_string(),
        protocol_version: MCP_PROTOCOL_VERSION,
        transport: MCP_TRANSPORT,
        server_url: format!("{base}/mcp"),
        auth: auth_for_mode(meta.mode, base),
        tools,
    }
}

fn auth_for_mode(mode: Mode, base: &str) -> AuthInfo {
    match mode {
        Mode::Personal => AuthInfo {
            kind: "none",
            resource_metadata: None,
        },
        Mode::MultiUser => AuthInfo {
            kind: "oauth2",
            // Forward-reference to the resource-metadata document
            // #7.4 lands at `/.well-known/oauth-protected-resource`.
            // Advertising it here even before that route exists is
            // safe — the discovery contract is "this is where to
            // look", not "this is currently populated"; a 404 from
            // the pointer is a recoverable client signal.
            resource_metadata: Some(format!("{base}/.well-known/oauth-protected-resource")),
        },
    }
}

/// Serialisable manifest body. Field names match the MCP discovery
/// draft convention (`snake_case`, optional `auth.resource_metadata`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub license: String,
    pub protocol_version: &'static str,
    pub transport: &'static str,
    pub server_url: String,
    pub auth: AuthInfo,
    pub tools: Vec<ToolSummary>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AuthInfo {
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_metadata: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ToolSummary {
    pub name: String,
    pub description: String,
}

// -------- Google A2A agent card (#7.3) ---------------------------------

/// Pure builder for the Google A2A agent card. Every MCP tool is
/// surfaced as a skill — the A2A "skill" abstraction maps 1:1 onto
/// our tool registry, so the catalogue stays unified between the
/// `/mcp.json` manifest and this card.
fn build_agent_card(dispatcher: &Dispatcher, meta: &ManifestMeta) -> AgentCard {
    let base = meta.public_base_url.trim_end_matches('/');
    let skills = dispatcher
        .list_tools()
        .into_iter()
        .map(|td| AgentSkill {
            id: td.name.clone(),
            name: td.name,
            description: td.description,
            // Tag every skill as MCP so an A2A client can filter
            // for the bridged surface; richer per-tool tags can
            // land later when the tool descriptors carry them.
            tags: vec!["mcp".to_string()],
            examples: Vec::new(),
        })
        .collect();
    AgentCard {
        name: meta.server_name.clone(),
        description: meta.description.clone(),
        url: base.to_string(),
        version: meta.server_version.to_string(),
        provider: AgentProvider {
            organization: meta.provider_organization.clone(),
            url: meta.provider_url.clone(),
        },
        documentation_url: meta.documentation_url.clone(),
        // Stream + push-notifications + state-transition history
        // are all `false` because the gateway speaks one-shot
        // request/response over Streamable HTTP MCP. Flipping
        // them to `true` would falsely advertise capabilities
        // the runtime doesn't actually implement.
        capabilities: AgentCapabilities {
            streaming: false,
            push_notifications: false,
            state_transition_history: false,
        },
        // Plain text in / structured JSON out matches what the
        // MCP `tools/call` JSON-RPC method actually accepts and
        // returns; per the A2A spec, additional modes can be
        // listed per-skill if a tool ever takes binary input.
        default_input_modes: vec!["text/plain".to_string()],
        default_output_modes: vec!["application/json".to_string(), "text/plain".to_string()],
        skills,
    }
}

/// Google A2A agent card (per
/// <https://google-a2a.github.io/A2A/specification/#agent-card>).
/// Field names serialise as `camelCase` to match the A2A schema.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentCard {
    pub name: String,
    pub description: String,
    pub url: String,
    pub version: String,
    pub provider: AgentProvider,
    pub documentation_url: String,
    pub capabilities: AgentCapabilities,
    pub default_input_modes: Vec<String>,
    pub default_output_modes: Vec<String>,
    pub skills: Vec<AgentSkill>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentProvider {
    pub organization: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    pub streaming: bool,
    pub push_notifications: bool,
    pub state_transition_history: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub examples: Vec<String>,
}

// -------- Skill index (#7.3) -------------------------------------------

/// Pure builder for the `/.well-known/agent-skills.json` document.
/// Indexes A2A "skill" names → MCP tool ids; today every skill maps
/// 1:1 onto the same-named MCP tool, but the indirection means a
/// future refactor can rename or group skills without forcing
/// agents to update their wiring.
fn build_skills_index(dispatcher: &Dispatcher) -> SkillsIndex {
    let skills = dispatcher
        .list_tools()
        .into_iter()
        .map(|td| (td.name.clone(), td.name))
        .collect();
    SkillsIndex {
        version: SKILLS_INDEX_VERSION,
        skills,
    }
}

/// Schema version of the skills index document. Bumped only when
/// the JSON shape changes in a way that requires consumer code
/// updates — additive changes (e.g. adding optional fields per
/// skill) stay on the current version.
const SKILLS_INDEX_VERSION: u32 = 1;

/// `/.well-known/agent-skills.json` body. `skills` keys are A2A
/// skill ids, values are MCP tool names that implement them.
/// `BTreeMap` keeps the serialisation order deterministic across
/// snapshots so the strong `ETag` only moves on a real registry
/// change.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SkillsIndex {
    pub version: u32,
    pub skills: std::collections::BTreeMap<String, String>,
}

// -------- RFC 9727 api-catalog (#7.4) ----------------------------------

/// Pure builder for the RFC 9727 API catalog document. Lists the
/// two public service surfaces the gateway exposes (MCP over
/// Streamable HTTP, and the REST API the `OpenAPI` doc at #7.5
/// describes) with cross-links to their respective discovery
/// payloads so an API-catalog-aware client can pivot from this
/// single entry point to a concrete service description.
fn build_api_catalog(meta: &ManifestMeta) -> ApiCatalog {
    let base = meta.public_base_url.trim_end_matches('/');
    ApiCatalog {
        linkset: vec![
            ApiCatalogEntry {
                anchor: format!("{base}/mcp"),
                service_desc: vec![LinkRef {
                    href: format!("{base}/.well-known/mcp.json"),
                    kind: "application/json".to_string(),
                }],
                service_doc: vec![LinkRef {
                    href: meta.documentation_url.clone(),
                    kind: "text/html".to_string(),
                }],
            },
            ApiCatalogEntry {
                anchor: format!("{base}/api"),
                // #7.5 will land /api/docs (Swagger UI) + the
                // OpenAPI JSON. Until then the cross-link 404s,
                // which is the documented "advertise the
                // pointer, let the resource 404 until it
                // exists" forward-reference shape per RFC 9727.
                service_desc: vec![LinkRef {
                    href: format!("{base}/api/openapi.json"),
                    kind: "application/json".to_string(),
                }],
                service_doc: vec![LinkRef {
                    href: format!("{base}/api/docs"),
                    kind: "text/html".to_string(),
                }],
            },
        ],
    }
}

/// RFC 9727 API catalog body. `linkset` is the array of API
/// entries (one per service surface). Schema:
/// <https://datatracker.ietf.org/doc/html/rfc9727>.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ApiCatalog {
    pub linkset: Vec<ApiCatalogEntry>,
}

/// One API entry in the catalog. `anchor` is the API's base URL;
/// the typed relation arrays (`service-desc`, `service-doc`) carry
/// the relation-typed links per RFC 8288 + RFC 9727.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ApiCatalogEntry {
    pub anchor: String,
    /// `service-desc` relation per IANA registry — link to a
    /// machine-readable service description (e.g. an `OpenAPI`
    /// document or the MCP manifest).
    #[serde(rename = "service-desc")]
    pub service_desc: Vec<LinkRef>,
    /// `service-doc` relation per IANA registry — link to a
    /// human-readable description of the service.
    #[serde(rename = "service-doc")]
    pub service_doc: Vec<LinkRef>,
}

/// Typed link object per RFC 8288 §3.1. `kind` serialises as
/// `type` to match the spec field name without shadowing Rust's
/// keyword.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LinkRef {
    pub href: String,
    #[serde(rename = "type")]
    pub kind: String,
}

// -------- RFC 9728 oauth-protected-resource (#7.4) ---------------------

/// Pure builder for the RFC 9728 OAuth Protected Resource metadata
/// document. Always emitted regardless of mode: in personal mode
/// the document advertises an empty `authorization_servers` array
/// (a spec-compliant signal that the resource doesn't delegate to
/// any AS); in multi-user mode operators set
/// `OAUTH_AUTHORIZATION_SERVERS` to the AS issuer URL(s).
fn build_oauth_resource(meta: &ManifestMeta) -> OAuthProtectedResource {
    let base = meta.public_base_url.trim_end_matches('/');
    OAuthProtectedResource {
        // The "resource identifier" per RFC 9728 §2 is the URL
        // the access token's `aud` claim must match. We use the
        // public base URL — `aud=https://hub.example` — which
        // matches the convention an MCP client sees when calling
        // `/mcp`.
        resource: base.to_string(),
        authorization_servers: meta.oauth_authorization_servers.clone(),
        // Per RFC 6750 the gateway accepts bearer tokens via the
        // `Authorization: Bearer …` header only — not via query
        // string (insecure, logged everywhere) or form body
        // (only meaningful for POST). One method is enough; if
        // we ever add form-body support we'd add `"body"` here.
        bearer_methods_supported: vec!["header".to_string()],
        // Pointer back to our own human-readable docs.
        resource_documentation: Some(meta.documentation_url.clone()),
        // Scopes the gateway advertises — kept abstract so the
        // multi-user OAuth integration can pick the actual
        // scope names without breaking discovery clients that
        // depend on this list.
        scopes_supported: vec!["mcp:read".to_string(), "mcp:write".to_string()],
    }
}

/// RFC 9728 OAuth Protected Resource Metadata body. Schema:
/// <https://datatracker.ietf.org/doc/html/rfc9728>.
///
/// Field name `kind`/`type` mismatches are deliberate: the spec
/// uses `snake_case` field names so no rename annotations are
/// needed. Optional fields use `Option` + `skip_serializing_if`
/// so the JSON output only carries set fields, matching the
/// "absence means default" convention.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OAuthProtectedResource {
    pub resource: String,
    pub authorization_servers: Vec<String>,
    pub bearer_methods_supported: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_documentation: Option<String>,
    pub scopes_supported: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
    use serde_json::{Map, Value, json};

    struct StubTool {
        name: &'static str,
        description: &'static str,
    }

    #[async_trait]
    impl ToolHandler for StubTool {
        fn descriptor(&self) -> ToolDescriptor {
            ToolDescriptor {
                name: self.name.to_string(),
                description: self.description.to_string(),
                input_schema: Map::from_iter([(
                    "type".to_string(),
                    Value::String("object".to_string()),
                )]),
                output_schema: None,
            }
        }
        async fn call(&self, _args: Value) -> Result<Value, ToolError> {
            Ok(json!(null))
        }
    }

    fn dispatcher_with(tools: Vec<(&'static str, &'static str)>) -> Dispatcher {
        let mut builder = Dispatcher::builder();
        for (name, description) in tools {
            builder = builder.register(StubTool { name, description });
        }
        builder.build()
    }

    fn meta(mode: Mode) -> ManifestMeta {
        ManifestMeta {
            public_base_url: "https://hub.example/".to_string(),
            server_name: "Test Hub".to_string(),
            description: "tagline".to_string(),
            server_version: "0.0.1",
            license: "Apache-2.0",
            mode,
            provider_organization: "Test Org".to_string(),
            provider_url: "https://example.test/org".to_string(),
            documentation_url: "https://example.test/docs".to_string(),
            oauth_authorization_servers: Vec::new(),
        }
    }

    #[test]
    fn manifest_personal_mode_omits_auth_resource_metadata() {
        let d = dispatcher_with(vec![("list_domains", "list domains")]);
        let m = build_manifest(&d, &meta(Mode::Personal));
        assert_eq!(m.auth.kind, "none");
        assert!(m.auth.resource_metadata.is_none());
        // `server_url` strips the trailing slash from `public_base_url`
        // so we don't emit `https://hub.example//mcp`.
        assert_eq!(m.server_url, "https://hub.example/mcp");
        assert_eq!(m.tools.len(), 1);
        assert_eq!(m.tools[0].name, "list_domains");
        assert_eq!(m.protocol_version, MCP_PROTOCOL_VERSION);
        assert_eq!(m.transport, MCP_TRANSPORT);
    }

    #[test]
    fn manifest_multiuser_mode_advertises_oauth2_with_resource_pointer() {
        let d = dispatcher_with(vec![("list_domains", "list domains")]);
        let m = build_manifest(&d, &meta(Mode::MultiUser));
        assert_eq!(m.auth.kind, "oauth2");
        assert_eq!(
            m.auth.resource_metadata.as_deref(),
            Some("https://hub.example/.well-known/oauth-protected-resource"),
        );
    }

    #[test]
    fn manifest_tools_summary_contains_descriptors_in_registry_order() {
        // Deliberately register out of sorted order so this test
        // actually catches a regression to insertion-order
        // iteration. With sorted registration the assertion would
        // pass even if `Dispatcher::list_tools` switched away from
        // `BTreeMap`.
        let d = dispatcher_with(vec![
            ("gamma", "gamma desc"),
            ("alpha", "alpha desc"),
            ("beta", "beta desc"),
        ]);
        let m = build_manifest(&d, &meta(Mode::Personal));
        // `Dispatcher::list_tools` iterates the inner `BTreeMap` by
        // name, so the manifest's `tools` list is alphabetically
        // sorted regardless of the order we registered them above.
        assert_eq!(
            m.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "beta", "gamma"],
        );
    }

    #[test]
    fn manifest_serialises_to_expected_json_shape() {
        let d = dispatcher_with(vec![("only", "only tool")]);
        let m = build_manifest(&d, &meta(Mode::Personal));
        let v: Value = serde_json::to_value(&m).unwrap();
        // Spot-check the keys the spec calls out — full equality
        // would be brittle to harmless reordering.
        let obj = v.as_object().unwrap();
        for key in [
            "name",
            "version",
            "description",
            "license",
            "protocol_version",
            "transport",
            "server_url",
            "auth",
            "tools",
        ] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
        // `auth.type` (not `kind`) — serde rename works.
        assert_eq!(obj["auth"]["type"], "none");
        // Personal mode must NOT include `resource_metadata` in the
        // serialised output (skip_serializing_if = Option::is_none).
        assert!(
            obj["auth"]
                .as_object()
                .unwrap()
                .get("resource_metadata")
                .is_none()
        );
    }

    #[test]
    fn etag_stable_for_identical_body() {
        let a = etag_for("body");
        let b = etag_for("body");
        let c = etag_for("BODY");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn if_none_match_wildcard_and_quoted_comma() {
        let stored = "\"abc\"";
        assert!(if_none_match_matches("*", stored));
        assert!(if_none_match_matches(stored, stored));
        assert!(if_none_match_matches("W/\"abc\"", stored));
        // Quoted-string carrying a literal comma must not split.
        let quoted_with_comma = "\"a,b\"";
        assert!(if_none_match_matches(quoted_with_comma, quoted_with_comma));
        // Non-matching tag.
        assert!(!if_none_match_matches("\"xyz\"", stored));
    }

    #[tokio::test]
    async fn router_serves_manifest_and_honours_if_none_match() {
        use axum::body::to_bytes;
        use axum::http::Request;
        use tower::ServiceExt as _;

        let d = dispatcher_with(vec![("only", "only tool")]);
        let app = router(&d, &meta(Mode::Personal));

        let resp = app
            .clone()
            .oneshot(
                Request::get("/.well-known/mcp.json")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let etag = resp
            .headers()
            .get(axum::http::header::ETAG)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["name"], "Test Hub");
        assert_eq!(v["tools"].as_array().unwrap().len(), 1);

        // Conditional GET with the prior ETag → 304.
        let resp = app
            .oneshot(
                Request::get("/.well-known/mcp.json")
                    .header(axum::http::header::IF_NONE_MATCH, etag)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 304);
    }

    // -------- agent-card / agent-skills (#7.3) -------------------------

    #[test]
    fn agent_card_contains_all_skills_with_mcp_tag() {
        let d = dispatcher_with(vec![
            ("gamma", "gamma desc"),
            ("alpha", "alpha desc"),
            ("beta", "beta desc"),
        ]);
        let card = build_agent_card(&d, &meta(Mode::Personal));
        assert_eq!(card.name, "Test Hub");
        // `url` strips the trailing slash on the public_base_url
        // so the rendered URL is canonical.
        assert_eq!(card.url, "https://hub.example");
        // Skills come from the dispatcher in alphabetical order
        // (BTreeMap), independent of registration order.
        assert_eq!(
            card.skills
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta", "gamma"],
        );
        for skill in &card.skills {
            // Every skill is tagged `mcp` so an A2A client can
            // filter for the bridged surface.
            assert!(skill.tags.contains(&"mcp".to_string()));
            // We don't fabricate examples — empty array per A2A
            // schema is preferable to lying about runtime output.
            assert!(skill.examples.is_empty());
            // id == name in this 1:1 mapping; the indirection
            // is preserved for future renames.
            assert_eq!(skill.id, skill.name);
        }
        // Capabilities all false — the gateway speaks one-shot
        // request/response, no streaming or push.
        assert!(!card.capabilities.streaming);
        assert!(!card.capabilities.push_notifications);
        assert!(!card.capabilities.state_transition_history);
    }

    #[test]
    fn agent_card_serialises_with_camelcase_keys() {
        let d = dispatcher_with(vec![("only", "only tool")]);
        let card = build_agent_card(&d, &meta(Mode::Personal));
        let v: Value = serde_json::to_value(&card).unwrap();
        let obj = v.as_object().unwrap();
        // A2A spec requires camelCase — pin the keys we
        // serde-renamed.
        assert!(obj.contains_key("documentationUrl"));
        assert!(obj.contains_key("defaultInputModes"));
        assert!(obj.contains_key("defaultOutputModes"));
        let caps = obj["capabilities"].as_object().unwrap();
        assert!(caps.contains_key("pushNotifications"));
        assert!(caps.contains_key("stateTransitionHistory"));
        // Common-case `name`/`url`/`version` stay as-is.
        assert!(obj.contains_key("name"));
        assert!(obj.contains_key("url"));
    }

    #[test]
    fn skills_index_maps_each_tool_to_itself_and_is_sorted() {
        let d = dispatcher_with(vec![
            ("gamma", "gamma desc"),
            ("alpha", "alpha desc"),
            ("beta", "beta desc"),
        ]);
        let idx = build_skills_index(&d);
        assert_eq!(idx.version, 1);
        let keys: Vec<&str> = idx.skills.keys().map(String::as_str).collect();
        // BTreeMap guarantees sorted iteration; the JSON output
        // therefore also sorts deterministically, which keeps the
        // strong ETag stable across registry-order shuffles.
        assert_eq!(keys, vec!["alpha", "beta", "gamma"]);
        for (k, v) in &idx.skills {
            assert_eq!(k, v, "skill id should map 1:1 onto the tool name");
        }
    }

    /// All five well-known surfaces the gateway mounts as of
    /// #7.4 — kept in one place so any future route addition
    /// shows up as a single edit.
    const ALL_WELL_KNOWN_PATHS: &[&str] = &[
        "/.well-known/mcp.json",
        "/.well-known/agent-card.json",
        "/.well-known/agent-skills.json",
        "/.well-known/api-catalog",
        "/.well-known/oauth-protected-resource",
    ];

    #[tokio::test]
    async fn router_serves_all_well_known_routes() {
        use axum::body::{Body, to_bytes};
        use axum::http::Request;
        use tower::ServiceExt as _;

        let d = dispatcher_with(vec![("only", "only tool")]);
        let app = router(&d, &meta(Mode::Personal));

        for path in ALL_WELL_KNOWN_PATHS {
            let resp = app
                .clone()
                .oneshot(Request::get(*path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), 200, "expected 200 for {path}");
            let etag = resp
                .headers()
                .get(axum::http::header::ETAG)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            assert!(!etag.is_empty(), "{path} missing ETag");
            let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
            let _v: Value = serde_json::from_slice(&body)
                .unwrap_or_else(|e| panic!("{path} body is not JSON: {e}"));
        }
    }

    #[tokio::test]
    async fn each_well_known_route_has_a_distinct_etag() {
        // Per-representation validators — a conditional GET on
        // one route MUST NOT return 304 for another route's body.
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt as _;

        let d = dispatcher_with(vec![("only", "only tool")]);
        let app = router(&d, &meta(Mode::Personal));

        let mut etags = Vec::new();
        for path in ALL_WELL_KNOWN_PATHS {
            let resp = app
                .clone()
                .oneshot(Request::get(*path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            etags.push(
                resp.headers()
                    .get(axum::http::header::ETAG)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned(),
            );
        }
        for i in 0..etags.len() {
            for j in (i + 1)..etags.len() {
                assert_ne!(
                    etags[i], etags[j],
                    "ETag collision between distinct well-known representations",
                );
            }
        }
    }

    // -------- RFC 9727 api-catalog (#7.4) ------------------------------

    #[test]
    fn api_catalog_lists_mcp_and_rest_entries() {
        let d = dispatcher_with(vec![("only", "only tool")]);
        let _ = d; // unused — catalog only depends on meta
        let catalog = build_api_catalog(&meta(Mode::Personal));
        assert_eq!(catalog.linkset.len(), 2, "expected MCP + REST entries");
        // MCP entry first, with the cross-link to the mcp.json
        // manifest as the service-desc.
        assert_eq!(catalog.linkset[0].anchor, "https://hub.example/mcp");
        assert_eq!(
            catalog.linkset[0].service_desc[0].href,
            "https://hub.example/.well-known/mcp.json",
        );
        // REST entry second, with forward-reference to #7.5
        // OpenAPI artefacts.
        assert_eq!(catalog.linkset[1].anchor, "https://hub.example/api");
        assert_eq!(
            catalog.linkset[1].service_desc[0].href,
            "https://hub.example/api/openapi.json",
        );
    }

    #[test]
    fn api_catalog_serialises_with_hyphenated_relation_names() {
        let catalog = build_api_catalog(&meta(Mode::Personal));
        let v: Value = serde_json::to_value(&catalog).unwrap();
        let linkset = v["linkset"].as_array().unwrap();
        let first = linkset[0].as_object().unwrap();
        // RFC 9727 / RFC 8288 use `service-desc` and
        // `service-doc` (hyphenated), not snake_case.
        assert!(first.contains_key("service-desc"));
        assert!(first.contains_key("service-doc"));
        // LinkRef `type` field — `kind` → `type` rename.
        let link = first["service-desc"][0].as_object().unwrap();
        assert!(link.contains_key("type"));
        assert!(link.contains_key("href"));
    }

    #[tokio::test]
    async fn api_catalog_response_uses_linkset_content_type() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt as _;

        let d = dispatcher_with(vec![("only", "only tool")]);
        let app = router(&d, &meta(Mode::Personal));
        let resp = app
            .oneshot(
                Request::get("/.well-known/api-catalog")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        // RFC 9727 §3 registers `application/linkset+json` as the
        // media type. Generic `application/json` would let a
        // content-negotiating intermediary mis-classify the
        // document.
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            "application/linkset+json",
        );
    }

    // -------- RFC 9728 oauth-protected-resource (#7.4) -----------------

    #[test]
    fn oauth_resource_personal_mode_has_empty_authorization_servers() {
        let resource = build_oauth_resource(&meta(Mode::Personal));
        assert_eq!(resource.resource, "https://hub.example");
        assert!(resource.authorization_servers.is_empty());
        assert_eq!(resource.bearer_methods_supported, vec!["header"]);
        assert!(resource.scopes_supported.contains(&"mcp:read".to_string()));
        assert!(resource.scopes_supported.contains(&"mcp:write".to_string()));
        assert!(resource.resource_documentation.is_some());
    }

    #[test]
    fn oauth_resource_multiuser_mode_reflects_configured_auth_servers() {
        let mut m = meta(Mode::MultiUser);
        m.oauth_authorization_servers = vec!["https://auth.hub.example".to_string()];
        let resource = build_oauth_resource(&m);
        assert_eq!(
            resource.authorization_servers,
            vec!["https://auth.hub.example".to_string()],
        );
    }

    #[test]
    fn oauth_resource_serialises_to_expected_shape() {
        let mut m = meta(Mode::MultiUser);
        m.oauth_authorization_servers = vec!["https://as.example".to_string()];
        let resource = build_oauth_resource(&m);
        let v: Value = serde_json::to_value(&resource).unwrap();
        let obj = v.as_object().unwrap();
        // RFC 9728 §2 mandates these fields with these names —
        // pin them so a future refactor can't accidentally rename
        // them out from under consumers.
        for key in [
            "resource",
            "authorization_servers",
            "bearer_methods_supported",
            "scopes_supported",
            "resource_documentation",
        ] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
        assert_eq!(obj["bearer_methods_supported"][0], "header");
    }
}
