//! `OpenAPI` 3.1 spec + Swagger UI / `ReDoc` browsable docs (#7.5).
//!
//! Builds an `OpenAPI` 3.1 document from the [`utoipa::ToSchema`] +
//! `#[utoipa::path]` annotations on the gateway's REST handlers,
//! serves the JSON spec at `/api/openapi.json`, and mounts the two
//! conventional browsable UIs:
//!
//! - **Swagger UI** at `/api/docs` — interactive "try-it-out"
//!   interface clients (and contributors) use to explore the API.
//! - **`ReDoc`** at `/api/redoc` — read-only narrative documentation
//!   that renders the same spec with a different visual style;
//!   linked from external developer docs that prefer `ReDoc`'s layout.
//!
//! All three surfaces are always-on; they describe handlers that
//! ship in every mode, and serving the docs doesn't require any DB
//! or session state.

use axum::Router;
use utoipa::OpenApi;
use utoipa_redoc::{Redoc, Servable as _};
use utoipa_swagger_ui::SwaggerUi;

use crate::api_routes;
use crate::marketplace_routes;

/// Root `OpenAPI` document. `utoipa::OpenApi` walks the `paths`
/// list to collect every annotated handler and folds in the
/// `components(schemas(...))` list so referenced response/body
/// types ship in the `#/components/schemas/...` section.
///
/// Session-gated routes (`/api/v1/me`, the api-keys CRUD) will
/// join as future PRs teach them about authentication metadata;
/// today only the public surfaces are documented.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Taiwan Data Hub Gateway",
        description = "REST control-plane endpoints exposed alongside the MCP server. \
The MCP protocol surface itself is documented separately at /.well-known/mcp.json.",
        license(name = "Apache-2.0", identifier = "Apache-2.0"),
    ),
    paths(
        api_routes::get_config,
        marketplace_routes::list_domains,
        marketplace_routes::list_datasets,
        marketplace_routes::get_dataset,
        marketplace_routes::list_collections,
        marketplace_routes::get_collection,
    ),
    components(schemas(
        api_routes::ConfigResponse,
        marketplace_routes::DomainResource,
        marketplace_routes::DatasetResource,
        marketplace_routes::DatasetResourceLink,
        marketplace_routes::CollectionResource,
        marketplace_routes::DomainListResponse,
        marketplace_routes::DatasetListResponse,
        marketplace_routes::CollectionListResponse,
    )),
    tags(
        (name = "config", description = "Gateway operating-mode discovery for the SvelteKit layout."),
        (name = "catalog", description = "Read-only marketplace catalog (domains, datasets, curated collections)."),
    ),
)]
pub struct ApiDoc;

/// Mount point for the `OpenAPI` JSON document. `pub` so
/// `main.rs` can echo the same literal into its
/// `ALWAYS_ON_PUBLIC_ROUTES` list without re-typing it — keeps
/// the router config and the boot-log/doctor view in lockstep
/// even when a future rename moves the path.
pub const OPENAPI_JSON_PATH: &str = "/api/openapi.json";

/// Mount point for the Swagger UI. Same shared-constant pattern
/// as [`OPENAPI_JSON_PATH`].
pub const SWAGGER_UI_PATH: &str = "/api/docs";

/// Mount point for the `ReDoc` UI. Same shared-constant pattern
/// as [`OPENAPI_JSON_PATH`].
pub const REDOC_PATH: &str = "/api/redoc";

/// Build the `OpenAPI` subrouter that serves the spec JSON +
/// Swagger UI + `ReDoc`. Each surface is composed from the same
/// [`ApiDoc`] so the three views can't drift out of sync.
///
/// Returns a `Router` ready to merge into the gateway's
/// top-level router. The router carries no shared state; each
/// inner service handles its own request envelope.
pub fn router() -> Router {
    let openapi = ApiDoc::openapi();
    // `SwaggerUi::new(...).url(...)` mounts both the UI assets
    // *and* the JSON spec the UI fetches. The `.url(path,
    // openapi)` tuple is what the UI's `urls` config consumes
    // at runtime — by reusing the same `OPENAPI_JSON_PATH` here
    // and below, browsers loading `/api/docs` ask for
    // `/api/openapi.json` exactly once and the JSON we serve
    // there is the one Swagger UI displays.
    let swagger = SwaggerUi::new(SWAGGER_UI_PATH).url(OPENAPI_JSON_PATH, openapi.clone());
    Router::new()
        .merge(swagger)
        .merge(Redoc::with_url(REDOC_PATH, openapi))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use serde_json::Value;
    use tower::ServiceExt as _;

    #[tokio::test]
    async fn openapi_spec_lists_get_config_path_with_3_1_version() {
        let app = router();
        let resp = app
            .oneshot(Request::get(OPENAPI_JSON_PATH).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        // Generous cap (16 MiB) so this test doesn't tip into
        // false failures as more endpoints + schemas get
        // annotated. The actual response today is a few KiB;
        // a single OpenAPI doc that exceeds 16 MiB would
        // signal we've inlined far too many large schemas
        // inline and should already be splitting them up.
        let bytes = to_bytes(resp.into_body(), 16 * 1024 * 1024).await.unwrap();
        let spec: Value = serde_json::from_slice(&bytes).unwrap();

        // utoipa 5.x emits an `OpenAPI` 3.1 document by default —
        // pin the version field so a future SDK upgrade that
        // silently dropped to 3.0 fires the test.
        let version = spec["openapi"].as_str().unwrap();
        assert!(
            version.starts_with("3.1"),
            "expected `OpenAPI` 3.1.x, got {version}",
        );
        // The annotated handlers must appear in the paths map;
        // each response body schema is referenced via `$ref` to
        // `#/components/schemas/...`.
        assert!(
            spec["paths"]["/api/v1/config"]["get"].is_object(),
            "missing GET /api/v1/config in spec",
        );
        assert!(
            spec["components"]["schemas"]["ConfigResponse"].is_object(),
            "missing ConfigResponse schema in components",
        );
        // #2.3 catalog surfaces — list + detail per resource.
        for expected in [
            "/api/v1/catalog/domains",
            "/api/v1/catalog/datasets",
            "/api/v1/catalog/datasets/{slug}",
            "/api/v1/catalog/collections",
            "/api/v1/catalog/collections/{slug}",
        ] {
            assert!(
                spec["paths"][expected]["get"].is_object(),
                "missing GET {expected} in spec",
            );
        }
        for schema in [
            "DomainResource",
            "DatasetResource",
            "DatasetResourceLink",
            "CollectionResource",
            "DomainListResponse",
            "DatasetListResponse",
            "CollectionListResponse",
        ] {
            assert!(
                spec["components"]["schemas"][schema].is_object(),
                "missing {schema} schema in components",
            );
        }
    }

    #[tokio::test]
    async fn swagger_ui_root_serves_html() {
        // Swagger UI mounts its index at `{SWAGGER_UI_PATH}/`
        // (trailing slash) and serves redirects + static assets
        // under that prefix. Build the request URL from the
        // shared constant so a rename of `SWAGGER_UI_PATH`
        // automatically reaches the test.
        let app = router();
        let path = format!("{SWAGGER_UI_PATH}/");
        let resp = app
            .oneshot(Request::get(&path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let ct = resp
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            ct.starts_with("text/html"),
            "expected text/html from swagger UI, got {ct}",
        );
    }

    #[tokio::test]
    async fn redoc_serves_html_at_mount_point() {
        let app = router();
        let resp = app
            .oneshot(Request::get(REDOC_PATH).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let ct = resp
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            ct.starts_with("text/html"),
            "expected text/html from ReDoc, got {ct}",
        );
    }
}
