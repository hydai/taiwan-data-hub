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

/// MIME type per RFC 8615 + the MCP discovery convention. Always
/// served as `application/json` regardless of mode.
const CONTENT_TYPE: &str = "application/json; charset=utf-8";

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
    /// Display name advertised in `name`. Falls through to a
    /// hard-coded default; an operator can override for branded
    /// deployments.
    pub server_name: String,
    /// Tagline rendered as `description`. Same default-with-override
    /// shape as `server_name`.
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
        }
    }
}

/// Pre-rendered manifest body + validator. Held behind `Arc` so
/// every HTTP request is a refcount bump rather than re-serialising
/// the JSON.
#[derive(Debug, Clone)]
struct ManifestState {
    body: Bytes,
    etag: String,
}

/// Build the `/.well-known/mcp.json` subrouter. Renders the body
/// once from the supplied [`Dispatcher`] snapshot + [`ManifestMeta`]
/// at construction time — the registry is fixed for the process
/// lifetime so a per-request rebuild would be wasted work.
pub fn router(dispatcher: &Dispatcher, meta: &ManifestMeta) -> Router {
    let manifest = build_manifest(dispatcher, meta);
    let body_json = serde_json::to_string_pretty(&manifest).expect("manifest serialises to JSON");
    let etag = etag_for(&body_json);
    let state = Arc::new(ManifestState {
        body: Bytes::from(body_json),
        etag,
    });
    Router::new()
        .route("/.well-known/mcp.json", get(handler))
        .with_state(state)
}

async fn handler(State(state): State<Arc<ManifestState>>, headers: HeaderMap) -> Response {
    if let Some(resp) = not_modified_if_match(&headers, &state.etag) {
        return resp;
    }
    success_response(state.body.clone(), &state.etag)
}

fn success_response(body: Bytes, etag: &str) -> Response {
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
        let d = dispatcher_with(vec![
            ("alpha", "alpha desc"),
            ("beta", "beta desc"),
            ("gamma", "gamma desc"),
        ]);
        let m = build_manifest(&d, &meta(Mode::Personal));
        // `Dispatcher::list_tools` iterates the inner `BTreeMap` by
        // name, so the manifest's `tools` list is deterministically
        // sorted regardless of registration order.
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
}
