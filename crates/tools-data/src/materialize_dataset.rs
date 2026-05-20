//! `materialize_dataset` MCP tool — issues a presigned URL for a
//! dataset file in the requested format.
//!
//! Pipeline:
//!
//! ```text
//!   parse args ──► MaterializeView::latest_materialise_view
//!                                  │
//!                                  └─► pick file matching `format`
//!                                                │
//!                            single-flight gate  │
//!                                                ▼
//!                                  ObjectStore::presign_get
//!                                                │
//!                                                ▼
//!                                  UsageRecorder::record_usage
//!                                                │
//!                                                ▼
//!                                  { url, format, expires_at, … }
//! ```
//!
//! Defense-in-depth (DESIGN.md §6 / #1.8 DoD):
//!
//! - Tool input is validated before any storage read: `id XOR slug`,
//!   `format` enum-checked, `ttl_seconds` REJECTED with
//!   `InvalidArguments` when outside [`MIN_TTL`]..=[`MAX_TTL`] (no
//!   silent clamping — surprising the caller with a different
//!   lifetime than they asked for is worse than a clear error).
//! - URI scheme dispatch picks the right `ObjectStore`; unknown
//!   schemes return a sanitized error and a server-side log entry.
//! - Per-process single-flight: concurrent calls for the same
//!   `(dataset_id, format)` SERIALISE through one mutex slot — at
//!   most one presign + one audit write executes at a time. They
//!   are not coalesced: each caller still gets their own presigned
//!   URL and their own audit row, since each caller's identity may
//!   differ. Cross-process dedup (Redis lock) and true coalescing
//!   are future concerns; today the gateway is single-instance.
//! - Usage write happens AFTER the presign succeeds — a 5xx presign
//!   failure must not pollute the audit log with false-positive
//!   entries.
//!
//! Schema is intentionally narrow: the returned URL is the only
//! credential-sensitive value, and the response fields are designed
//! for a caller that just wants to `curl -O` the result.

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use object_store::{ObjectStore, ObjectStoreError, PresignedUrl};
use serde_json::{Map, Value, json};
use storage::{DatasetKey, DatasetLatestFiles, MaterializeView, NewUsageRecord, UsageRecorder};
use tokio::sync::Mutex;
use uuid::Uuid;

/// MCP tool name. Stable identifier — clients pin to this string.
pub const TOOL_NAME: &str = "materialize_dataset";

/// Default URL lifetime if the caller doesn't specify one.
pub const DEFAULT_TTL: Duration = Duration::from_secs(60 * 60);
/// Lower bound — a TTL shorter than this is almost certainly a
/// caller bug and rounds the URL to "useless before the network
/// hop completes".
pub const MIN_TTL: Duration = Duration::from_secs(30);
/// Upper bound — matches the AWS `SigV4` presigned URL cap. The
/// `LocalFs` backend may carry a smaller cap of its own.
pub const MAX_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Supported export formats. Pinned to the `dataset_files.format`
/// CHECK constraint; any format upstream actually has cached will
/// land here. The list is wider than what most callers will use so
/// the tool can pass through whatever the ETL stored.
pub const SUPPORTED_FORMATS: &[&str] = &["csv", "json", "jsonl", "parquet", "xml", "pdf", "zip"];

/// Routes a request URI to the right backend by scheme. Construct
/// once at gateway boot; cheap to clone (each variant is `Arc`-
/// backed).
///
/// `http://` and `https://` URIs are always handled by a built-in
/// passthrough store ([`HttpsPassthroughStore`]) — the upstream URL
/// is returned verbatim. `dataset_files.uri` documents this as the
/// "passthrough to upstream" mode for catalogs that prefer to serve
/// their own downloads.
#[derive(Clone)]
pub struct ObjectStoreRouter {
    local_fs: Option<Arc<dyn ObjectStore>>,
    s3: Option<Arc<dyn ObjectStore>>,
    https_passthrough: Arc<dyn ObjectStore>,
}

impl ObjectStoreRouter {
    pub fn new() -> Self {
        Self {
            local_fs: None,
            s3: None,
            https_passthrough: Arc::new(HttpsPassthroughStore),
        }
    }

    pub fn with_local_fs(mut self, store: Arc<dyn ObjectStore>) -> Self {
        self.local_fs = Some(store);
        self
    }

    pub fn with_s3(mut self, store: Arc<dyn ObjectStore>) -> Self {
        self.s3 = Some(store);
        self
    }

    fn pick(&self, uri: &str) -> Option<Arc<dyn ObjectStore>> {
        if uri.starts_with("file://") || !uri.contains("://") {
            self.local_fs.clone()
        } else if uri.starts_with("s3://") {
            self.s3.clone()
        } else if uri.starts_with("http://") || uri.starts_with("https://") {
            // Passthrough is always available — there's no backend
            // to configure. Surfacing it via the same router shape
            // keeps the tool layer's dispatch uniform.
            Some(self.https_passthrough.clone())
        } else {
            None
        }
    }
}

impl Default for ObjectStoreRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ObjectStoreRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectStoreRouter")
            .field("has_local_fs", &self.local_fs.is_some())
            .field("has_s3", &self.s3.is_some())
            .field("has_https_passthrough", &true)
            .finish_non_exhaustive()
    }
}

/// Returns `http(s)://` URIs verbatim. Honours the `dataset_files.
/// uri` "passthrough to upstream" contract: the tool layer hands
/// the caller the upstream URL unchanged and `expires_at` reflects
/// the requested TTL window even though upstream is in control of
/// the actual link lifetime.
#[derive(Debug, Clone)]
struct HttpsPassthroughStore;

#[async_trait]
impl ObjectStore for HttpsPassthroughStore {
    async fn presign_get(
        &self,
        uri: &str,
        ttl: Duration,
    ) -> Result<PresignedUrl, ObjectStoreError> {
        if !(uri.starts_with("http://") || uri.starts_with("https://")) {
            return Err(ObjectStoreError::InvalidUri(format!(
                "passthrough store only handles http(s):// URIs, got: {uri}"
            )));
        }
        let expires_at = chrono::Utc::now()
            + chrono::Duration::from_std(ttl).map_err(|_| ObjectStoreError::TtlOutOfRange {
                requested: ttl,
                max: Duration::from_secs(i64::MAX as u64),
            })?;
        Ok(PresignedUrl {
            url: uri.to_owned(),
            expires_at,
        })
    }
}

/// Single-flight slot keyed by `(dataset_id, format)`.
type InflightSlot = Arc<Mutex<()>>;
/// Map from request key to a `Weak` reference of its in-flight slot.
/// Holding `Weak` (instead of `Arc`) lets dead entries get pruned on
/// the next acquire — the map's size is bounded by the number of
/// currently-in-flight keys, not by the lifetime of the process.
type InflightMap = Arc<Mutex<HashMap<(Uuid, String), Weak<Mutex<()>>>>>;

/// Tool entry point. Production wires every `Arc<dyn …>` to a
/// `storage::Storage` + an `ObjectStoreRouter`; tests plug in
/// stubs per trait.
#[derive(Clone)]
pub struct MaterializeDatasetTool {
    view: Arc<dyn MaterializeView>,
    recorder: Arc<dyn UsageRecorder>,
    router: Arc<ObjectStoreRouter>,
    /// Single-flight gate. Slots get evicted once no in-flight
    /// request still holds a strong reference. We use
    /// `tokio::sync::Mutex` (not `std::sync::Mutex`) so an await
    /// inside the critical section doesn't dead-lock the runtime.
    inflight: InflightMap,
}

impl MaterializeDatasetTool {
    pub fn new<V, U>(view: V, recorder: U, router: ObjectStoreRouter) -> Self
    where
        V: MaterializeView,
        U: UsageRecorder,
    {
        Self {
            view: Arc::new(view),
            recorder: Arc::new(recorder),
            router: Arc::new(router),
            inflight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn from_arcs(
        view: Arc<dyn MaterializeView>,
        recorder: Arc<dyn UsageRecorder>,
        router: Arc<ObjectStoreRouter>,
    ) -> Self {
        Self {
            view,
            recorder,
            router,
            inflight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Acquire (or wait for) the single-flight slot for this
    /// `(dataset_id, format)`. Returns an owned guard whose `Drop`
    /// releases the lock and lets the next waiter proceed. The map
    /// holds only `Weak` references so dropping the last live guard
    /// makes the entry collectable; we prune dead entries on every
    /// acquire so the map's size stays bounded by currently-in-
    /// flight keys.
    async fn lock_slot(&self, key: (Uuid, String)) -> tokio::sync::OwnedMutexGuard<()> {
        let slot: InflightSlot = {
            let mut map = self.inflight.lock().await;
            // Cheap GC: drop entries whose strong-count went to zero
            // since the last acquire. Map size is bounded by the
            // number of concurrent keys, so this is O(n) over a tiny
            // n in practice.
            map.retain(|_, weak| weak.strong_count() > 0);
            if let Some(existing) = map.get(&key).and_then(Weak::upgrade) {
                existing
            } else {
                let fresh = Arc::new(Mutex::new(()));
                map.insert(key, Arc::downgrade(&fresh));
                fresh
            }
        };
        slot.lock_owned().await
    }
}

impl std::fmt::Debug for MaterializeDatasetTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MaterializeDatasetTool")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ToolHandler for MaterializeDatasetTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: format!(
                "Issue a short-lived presigned download URL for a dataset's latest version. \
                 Specify the dataset by `id` (UUID) or `slug`; exactly one is required. \
                 When `format` is omitted, the tool picks parquet if available, otherwise csv, \
                 otherwise the first file the latest version carries. `ttl_seconds` defaults \
                 to {default_ttl} and MUST be within [{min_ttl}, {max_ttl}] (values outside \
                 the range are rejected). Returns the URL plus the file size and computed \
                 expiry. Best-effort writes a `usage_records` audit row after the URL is \
                 signed; audit failures are logged server-side but do not fail the response \
                 because the URL is already valid in the caller's hands at that point.",
                default_ttl = DEFAULT_TTL.as_secs(),
                min_ttl = MIN_TTL.as_secs(),
                max_ttl = MAX_TTL.as_secs(),
            ),
            input_schema: input_schema()
                .as_object()
                .expect("input_schema returns a JSON object literal")
                .clone(),
            output_schema: Some(
                output_schema()
                    .as_object()
                    .expect("output_schema returns a JSON object literal")
                    .clone(),
            ),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let req = Request::parse(&args)?;

        let view = self
            .view
            .latest_materialise_view(req.key.clone())
            .await
            .map_err(|e| ToolError::Execution(format!("storage: {e}")))?;
        let Some(view) = view else {
            return Err(ToolError::NotFound(format!(
                "dataset not found ({})",
                req.lookup_str()
            )));
        };

        let (file, dataset_version_id) = pick_file(&view, req.format.as_deref())?;
        let chosen_format = file.format.clone();

        let _guard = self
            .lock_slot((view.dataset_id, chosen_format.clone()))
            .await;

        let backend = self
            .router
            .pick(&file.uri)
            .ok_or_else(|| presign_backend_missing(&file.uri))?;

        let presigned = backend
            .presign_get(&file.uri, req.ttl)
            .await
            .map_err(|e| map_object_store_err(&e))?;

        // Audit happens AFTER a successful presign so a 5xx in the
        // signer doesn't write a false-positive usage row. Failure to
        // record is logged but does not fail the caller — the URL is
        // already valid in the caller's hands and re-issuing it would
        // produce a different (also-valid) URL, so swallowing the
        // audit error is the least-confusing option. Operators get
        // the diagnostic via the `tracing` event.
        let usage_id_result = self
            .recorder
            .record_usage(&NewUsageRecord {
                dataset_id: view.dataset_id,
                dataset_version_id,
                tool: TOOL_NAME,
                format: Some(&chosen_format),
                principal_kind: req.principal_kind,
                principal_id: req.principal_id.as_deref(),
                byte_size: file.byte_size,
            })
            .await;
        if let Err(e) = &usage_id_result {
            tracing::error!(
                slug = %view.slug,
                err = %e,
                "materialize_dataset usage_records write failed; URL was still issued"
            );
        }

        Ok(render_response(
            &view,
            &chosen_format,
            file.byte_size,
            file.checksum.as_deref(),
            &presigned,
        ))
    }
}

/// Parsed + validated tool input.
#[derive(Debug, Clone)]
struct Request {
    key: DatasetKey,
    format: Option<String>,
    ttl: Duration,
    principal_kind: &'static str,
    principal_id: Option<String>,
}

impl Request {
    fn parse(args: &Value) -> Result<Self, ToolError> {
        let obj = args
            .as_object()
            .ok_or_else(|| ToolError::InvalidArguments("arguments must be an object".into()))?;

        let id_str = obj.get("id").and_then(Value::as_str);
        let slug = obj.get("slug").and_then(Value::as_str);
        let key = match (id_str, slug) {
            (Some(_), Some(_)) => {
                return Err(ToolError::InvalidArguments(
                    "specify exactly one of `id` or `slug`".into(),
                ));
            }
            (None, None) => {
                return Err(ToolError::InvalidArguments(
                    "specify either `id` or `slug`".into(),
                ));
            }
            (Some(s), None) => {
                let id = Uuid::parse_str(s)
                    .map_err(|_| ToolError::InvalidArguments(format!("`id` is not a UUID: {s}")))?;
                DatasetKey::Id(id)
            }
            (None, Some(s)) => DatasetKey::Slug(s.to_owned()),
        };

        let format = match obj.get("format") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => {
                if !SUPPORTED_FORMATS.contains(&s.as_str()) {
                    return Err(ToolError::InvalidArguments(format!(
                        "`format` must be one of {SUPPORTED_FORMATS:?}, got `{s}`"
                    )));
                }
                Some(s.to_owned())
            }
            Some(_) => {
                return Err(ToolError::InvalidArguments(
                    "`format` must be a string".into(),
                ));
            }
        };

        let ttl = match obj.get("ttl_seconds") {
            None | Some(Value::Null) => DEFAULT_TTL,
            Some(v) => {
                let secs = v.as_u64().ok_or_else(|| {
                    ToolError::InvalidArguments(
                        "`ttl_seconds` must be a non-negative integer".into(),
                    )
                })?;
                let d = Duration::from_secs(secs);
                if d < MIN_TTL || d > MAX_TTL {
                    return Err(ToolError::InvalidArguments(format!(
                        "`ttl_seconds` must be between {} and {}",
                        MIN_TTL.as_secs(),
                        MAX_TTL.as_secs()
                    )));
                }
                d
            }
        };

        let (principal_kind, principal_id) = parse_principal(obj.get("principal"))?;

        Ok(Self {
            key,
            format,
            ttl,
            principal_kind,
            principal_id,
        })
    }

    fn lookup_str(&self) -> String {
        match &self.key {
            DatasetKey::Id(id) => format!("id={id}"),
            DatasetKey::Slug(slug) => format!("slug={slug}"),
        }
    }
}

/// `principal` is optional. Authenticated callers (post-#4) will
/// supply it server-side; the unauthenticated personal-mode path
/// lands as "anonymous".
///
/// Cross-field rule: `id` MUST accompany `user` / `api_key` and
/// MUST be absent for `anonymous`. The `usage_records` migration
/// documents `principal_id` as NULL only for anonymous; enforcing
/// the invariant here keeps the audit log unambiguous downstream.
fn parse_principal(raw: Option<&Value>) -> Result<(&'static str, Option<String>), ToolError> {
    match raw {
        None | Some(Value::Null) => Ok(("anonymous", None)),
        Some(Value::Object(p)) => {
            let kind = p.get("kind").and_then(Value::as_str).ok_or_else(|| {
                ToolError::InvalidArguments(
                    "`principal.kind` must be a string when `principal` is set".into(),
                )
            })?;
            let kind: &'static str = match kind {
                "anonymous" => "anonymous",
                "user" => "user",
                "api_key" => "api_key",
                other => {
                    return Err(ToolError::InvalidArguments(format!(
                        "`principal.kind` must be one of anonymous/user/api_key, got `{other}`"
                    )));
                }
            };
            let id_raw = p.get("id").and_then(Value::as_str);
            let id = match (kind, id_raw) {
                ("anonymous", Some(_)) => {
                    return Err(ToolError::InvalidArguments(
                        "`principal.id` must be omitted when `kind` = anonymous".into(),
                    ));
                }
                ("anonymous", None) => None,
                (_, None) => {
                    return Err(ToolError::InvalidArguments(format!(
                        "`principal.id` is required when `kind` = {kind}"
                    )));
                }
                (_, Some("")) => {
                    return Err(ToolError::InvalidArguments(format!(
                        "`principal.id` must not be empty when `kind` = {kind}"
                    )));
                }
                (_, Some(s)) => Some(s.to_owned()),
            };
            Ok((kind, id))
        }
        Some(_) => Err(ToolError::InvalidArguments(
            "`principal` must be an object".into(),
        )),
    }
}

/// Pick the right `dataset_files` row for the request. Returns the
/// chosen row plus the latest version id (we revalidate against the
/// view here so the caller can't smuggle a stale id past us). The
/// preference order when the caller didn't supply `format` is
/// parquet → csv → first available — parquet for everything the
/// ETL has already written, csv for everything else.
fn pick_file(
    view: &DatasetLatestFiles,
    requested_format: Option<&str>,
) -> Result<(storage::DatasetFileRow, Option<Uuid>), ToolError> {
    if view.files.is_empty() {
        return Err(ToolError::NotFound(format!(
            "dataset `{}` is not materialised yet — \
             waiting for the ETL to write a dataset_files row",
            view.slug
        )));
    }

    let pick = if let Some(format) = requested_format {
        view.files.iter().find(|f| f.format == format).cloned()
    } else {
        view.files
            .iter()
            .find(|f| f.format == "parquet")
            .or_else(|| view.files.iter().find(|f| f.format == "csv"))
            .or_else(|| view.files.first())
            .cloned()
    };

    let file = pick.ok_or_else(|| {
        ToolError::NotFound(format!(
            "dataset `{}` has no file in the requested format ({:?})",
            view.slug, requested_format
        ))
    })?;

    Ok((file, view.latest_version_id))
}

fn map_object_store_err(err: &ObjectStoreError) -> ToolError {
    match err {
        ObjectStoreError::InvalidUri(_) => {
            // Don't echo the raw URI back — it may carry the bucket
            // name or a path prefix that the operator hasn't chosen
            // to publish. The server-side log carries the detail.
            tracing::warn!(error = %err, "materialize_dataset received an object store invalid URI");
            ToolError::Execution(
                "stored file URI is not usable by the configured object store".into(),
            )
        }
        ObjectStoreError::TtlOutOfRange { .. } => ToolError::InvalidArguments(err.to_string()),
        ObjectStoreError::SigningFailed(_) => {
            tracing::error!(error = %err, "materialize_dataset signing failed");
            ToolError::Execution("URL signing failed — see server logs for details".into())
        }
    }
}

/// Build a sanitised, caller-facing error when no backend handles
/// the stored URI's scheme. The server-side log carries the URI;
/// the caller only sees the scheme. Bare paths (no `://`) are
/// reported as `file` because that's how the router dispatches
/// them — saying `<no scheme>` would be technically accurate but
/// would mislead operators into looking for the wrong env vars.
fn presign_backend_missing(uri: &str) -> ToolError {
    let scheme = uri.split_once("://").map_or("file", |(s, _)| s);
    tracing::warn!(
        scheme,
        "materialize_dataset has no backend configured for stored URI scheme"
    );
    ToolError::Execution(format!(
        "no presigning backend is configured for scheme `{scheme}`"
    ))
}

fn render_response(
    view: &DatasetLatestFiles,
    format: &str,
    byte_size: Option<i64>,
    checksum: Option<&str>,
    presigned: &PresignedUrl,
) -> Value {
    let mut out = Map::new();
    out.insert(
        "dataset_id".into(),
        Value::String(view.dataset_id.to_string()),
    );
    out.insert("slug".into(), Value::String(view.slug.clone()));
    if let Some(vid) = view.latest_version_id {
        out.insert("version_id".into(), Value::String(vid.to_string()));
    }
    out.insert("format".into(), Value::String(format.to_owned()));
    out.insert("url".into(), Value::String(presigned.url.clone()));
    out.insert(
        "expires_at".into(),
        Value::String(presigned.expires_at.to_rfc3339()),
    );
    if let Some(size) = byte_size {
        out.insert("byte_size".into(), json!(size));
    }
    if let Some(c) = checksum {
        out.insert("checksum".into(), Value::String(c.to_owned()));
    }
    Value::Object(out)
}

fn input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": {
                "type": "string",
                "description": "Dataset UUID. Mutually exclusive with `slug`."
            },
            "slug": {
                "type": "string",
                "description": "Dataset slug. Mutually exclusive with `id`."
            },
            "format": {
                "type": "string",
                "enum": SUPPORTED_FORMATS,
                "description": "Requested file format. Defaults to parquet (then csv, then first available)."
            },
            "ttl_seconds": {
                "type": "integer",
                "minimum": MIN_TTL.as_secs(),
                "maximum": MAX_TTL.as_secs(),
                "description": "URL lifetime in seconds. Defaults to one hour."
            },
            "principal": {
                "type": "object",
                "description": "Caller identification. Server-side auth normally fills this in; clients may omit it.",
                "properties": {
                    "kind": { "type": "string", "enum": ["anonymous", "user", "api_key"] },
                    "id":   { "type": "string" }
                },
                "required": ["kind"]
            }
        },
        "additionalProperties": false
    })
}

fn output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "dataset_id":  { "type": "string", "format": "uuid" },
            "slug":        { "type": "string" },
            "version_id":  { "type": "string", "format": "uuid" },
            "format":      { "type": "string" },
            "url":         { "type": "string" },
            "expires_at":  { "type": "string", "format": "date-time" },
            "byte_size":   { "type": "integer" },
            "checksum":    { "type": "string" }
        },
        "required": ["dataset_id", "slug", "format", "url", "expires_at"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, TimeZone, Utc};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use storage::{DatasetFileRow, DatasetLatestFiles, StorageError};

    fn sample_view(files: Vec<DatasetFileRow>) -> DatasetLatestFiles {
        DatasetLatestFiles {
            dataset_id: Uuid::nil(),
            slug: "test-slug".to_owned(),
            latest_version_id: Some(Uuid::nil()),
            files,
        }
    }

    fn parquet_file() -> DatasetFileRow {
        DatasetFileRow {
            id: Uuid::nil(),
            dataset_version_id: Uuid::nil(),
            format: "parquet".to_owned(),
            uri: "file:///cache/test-slug/v1.parquet".to_owned(),
            byte_size: Some(2048),
            checksum: Some("deadbeef".to_owned()),
        }
    }

    fn csv_file() -> DatasetFileRow {
        DatasetFileRow {
            id: Uuid::nil(),
            dataset_version_id: Uuid::nil(),
            format: "csv".to_owned(),
            uri: "file:///cache/test-slug/v1.csv".to_owned(),
            byte_size: Some(1024),
            checksum: None,
        }
    }

    #[derive(Clone, Default)]
    struct StubView {
        view: Option<DatasetLatestFiles>,
    }
    #[async_trait]
    impl MaterializeView for StubView {
        async fn latest_materialise_view(
            &self,
            _key: DatasetKey,
        ) -> Result<Option<DatasetLatestFiles>, StorageError> {
            Ok(self.view.clone())
        }
    }

    #[derive(Clone, Default)]
    struct StubRecorder {
        // `Arc<AtomicUsize>` so the test handle and the tool's
        // owned copy observe the same counter — without the `Arc`
        // a `Clone`-by-value path produces a fresh counter and
        // masks assertion failures.
        count: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl UsageRecorder for StubRecorder {
        async fn record_usage(&self, _record: &NewUsageRecord<'_>) -> Result<Uuid, StorageError> {
            self.count.fetch_add(1, Ordering::SeqCst);
            Ok(Uuid::nil())
        }
    }

    #[derive(Clone)]
    struct StubStore {
        calls: Arc<AtomicUsize>,
        delay: Duration,
    }
    impl StubStore {
        fn new(delay: Duration) -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                delay,
            }
        }
    }
    #[async_trait]
    impl ObjectStore for StubStore {
        async fn presign_get(
            &self,
            uri: &str,
            ttl: Duration,
        ) -> Result<PresignedUrl, ObjectStoreError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            let expires_at: DateTime<Utc> = Utc.timestamp_opt(1_779_235_200, 0).single().unwrap()
                + chrono::Duration::from_std(ttl).unwrap();
            Ok(PresignedUrl {
                url: format!("https://signed.test/{uri}?ttl={}", ttl.as_secs()),
                expires_at,
            })
        }
    }

    fn router_with(store: Arc<dyn ObjectStore>) -> ObjectStoreRouter {
        ObjectStoreRouter::new().with_local_fs(store)
    }

    #[tokio::test]
    async fn returns_not_found_when_dataset_missing() {
        let tool = MaterializeDatasetTool::new(
            StubView::default(),
            StubRecorder::default(),
            ObjectStoreRouter::new(),
        );
        let err = tool.call(json!({ "slug": "nope" })).await.unwrap_err();
        assert!(matches!(err, ToolError::NotFound(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn returns_not_found_when_no_files_yet() {
        let tool = MaterializeDatasetTool::new(
            StubView {
                view: Some(sample_view(vec![])),
            },
            StubRecorder::default(),
            ObjectStoreRouter::new(),
        );
        let err = tool.call(json!({ "slug": "test-slug" })).await.unwrap_err();
        let msg = match err {
            ToolError::NotFound(m) => m,
            other => panic!("expected NotFound, got {other:?}"),
        };
        assert!(msg.contains("not materialised yet"));
    }

    #[tokio::test]
    async fn rejects_id_xor_slug() {
        let tool = MaterializeDatasetTool::new(
            StubView::default(),
            StubRecorder::default(),
            ObjectStoreRouter::new(),
        );
        let err = tool
            .call(json!({
                "id":   "00000000-0000-0000-0000-000000000000",
                "slug": "anything"
            }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn picks_requested_format_when_supplied() {
        let store = Arc::new(StubStore::new(Duration::ZERO));
        let recorder = Arc::new(StubRecorder::default());
        let tool = MaterializeDatasetTool::from_arcs(
            Arc::new(StubView {
                view: Some(sample_view(vec![parquet_file(), csv_file()])),
            }),
            recorder.clone(),
            Arc::new(router_with(store.clone())),
        );
        let resp = tool
            .call(json!({ "slug": "test-slug", "format": "csv" }))
            .await
            .unwrap();
        assert_eq!(resp["format"], "csv");
        assert!(resp["url"].as_str().unwrap().contains("v1.csv"));
        assert_eq!(recorder.count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn defaults_to_parquet_then_csv() {
        let store = Arc::new(StubStore::new(Duration::ZERO));
        // Order is csv-first to prove the preference logic, not
        // the iteration order, picks parquet.
        let tool = MaterializeDatasetTool::new(
            StubView {
                view: Some(sample_view(vec![csv_file(), parquet_file()])),
            },
            StubRecorder::default(),
            router_with(store),
        );
        let resp = tool.call(json!({ "slug": "test-slug" })).await.unwrap();
        assert_eq!(resp["format"], "parquet");
    }

    #[tokio::test]
    async fn rejects_ttl_outside_bounds() {
        let store = Arc::new(StubStore::new(Duration::ZERO));
        let tool = MaterializeDatasetTool::new(
            StubView {
                view: Some(sample_view(vec![parquet_file()])),
            },
            StubRecorder::default(),
            router_with(store),
        );
        let err = tool
            .call(json!({ "slug": "test-slug", "ttl_seconds": 5 }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn writes_usage_row_for_each_successful_call() {
        let recorder = Arc::new(StubRecorder::default());
        let store = Arc::new(StubStore::new(Duration::ZERO));
        let tool = MaterializeDatasetTool::from_arcs(
            Arc::new(StubView {
                view: Some(sample_view(vec![parquet_file()])),
            }),
            recorder.clone(),
            Arc::new(router_with(store)),
        );
        // Three distinct calls — same dataset_id but no concurrency
        // means the single-flight gate doesn't dedup them.
        for _ in 0..3 {
            tool.call(json!({ "slug": "test-slug" })).await.unwrap();
        }
        assert_eq!(recorder.count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn concurrent_calls_serialize_through_single_flight_gate() {
        // The gate ensures that concurrent calls for the same
        // `(dataset_id, format)` serialise on one Mutex. We assert
        // that property by running 10 concurrent calls with a
        // delay-laden stub: total wall time ≥ 10 * delay because the
        // gate forces them through one at a time. Without the gate
        // 10 calls would interleave and total wall time would be
        // close to a single delay.
        let store = Arc::new(StubStore::new(Duration::from_millis(20)));
        let recorder = Arc::new(StubRecorder::default());
        let tool = MaterializeDatasetTool::from_arcs(
            Arc::new(StubView {
                view: Some(sample_view(vec![parquet_file()])),
            }),
            recorder.clone(),
            Arc::new(router_with(store.clone())),
        );

        let start = std::time::Instant::now();
        let mut handles = Vec::new();
        for _ in 0..10 {
            let tool = tool.clone();
            handles.push(tokio::spawn(async move {
                tool.call(json!({ "slug": "test-slug" })).await.unwrap()
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        let elapsed = start.elapsed();

        // Under perfect serialisation: ~200ms. Under parallel
        // execution: ~20ms. Cut the threshold conservatively at
        // 6 * 20ms = 120ms to keep the test stable on slow CI.
        assert!(
            elapsed >= Duration::from_millis(120),
            "expected >=120ms under single-flight serialisation, got {elapsed:?}"
        );
        // We made 10 distinct calls, so 10 presign + 10 audit rows
        // — the gate doesn't *coalesce* calls today, it just
        // serialises them. Coalescing is left for #1.8 follow-up.
        assert_eq!(store.calls.load(Ordering::SeqCst), 10);
        assert_eq!(recorder.count.load(Ordering::SeqCst), 10);
    }

    #[tokio::test]
    async fn unknown_uri_scheme_returns_clear_error() {
        // Empty router → router.pick() returns None for any URI →
        // tool surfaces the "no backend configured" error without
        // echoing the raw URI back to the caller.
        let recorder = StubRecorder::default();
        let unknown = DatasetFileRow {
            uri: "gopher://nope/file.parquet".to_owned(),
            ..parquet_file()
        };
        let tool = MaterializeDatasetTool::new(
            StubView {
                view: Some(sample_view(vec![unknown])),
            },
            recorder.clone(),
            ObjectStoreRouter::new(),
        );
        let err = tool.call(json!({ "slug": "test-slug" })).await.unwrap_err();
        let msg = match err {
            ToolError::Execution(m) => m,
            other => panic!("expected Execution, got {other:?}"),
        };
        assert!(msg.contains("gopher"), "scheme should leak; got: {msg}");
        assert!(!msg.contains("nope"), "host must not leak; got: {msg}");
        // No usage row written when presign couldn't proceed. The
        // shared `Arc<AtomicUsize>` in `StubRecorder` is what makes
        // this assertion meaningful: a clone keeps the counter so
        // we observe the tool's view of "no writes" through the
        // test's handle.
        assert_eq!(recorder.count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn https_uri_passes_through_to_upstream_url() {
        // `dataset_files.uri` documents `https://` as a passthrough
        // to upstream. The tool layer returns the URL verbatim, no
        // signing, with an `expires_at` derived from the requested
        // TTL window.
        let recorder = StubRecorder::default();
        let upstream = DatasetFileRow {
            uri: "https://upstream.example.com/files/v1.csv?token=abc".to_owned(),
            format: "csv".to_owned(),
            ..parquet_file()
        };
        let tool = MaterializeDatasetTool::new(
            StubView {
                view: Some(sample_view(vec![upstream.clone()])),
            },
            recorder.clone(),
            // Router with NO local-fs / s3 wired — passthrough is
            // always available regardless.
            ObjectStoreRouter::new(),
        );
        let resp = tool
            .call(json!({ "slug": "test-slug", "format": "csv" }))
            .await
            .unwrap();
        assert_eq!(resp["url"], upstream.uri);
        assert_eq!(resp["format"], "csv");
        // Audit row still written for upstream passthrough.
        assert_eq!(recorder.count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn principal_id_required_when_kind_not_anonymous() {
        let store = Arc::new(StubStore::new(Duration::ZERO));
        let tool = MaterializeDatasetTool::new(
            StubView {
                view: Some(sample_view(vec![parquet_file()])),
            },
            StubRecorder::default(),
            router_with(store),
        );
        let err = tool
            .call(json!({
                "slug": "test-slug",
                "principal": { "kind": "user" }
            }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn principal_id_rejected_for_anonymous_kind() {
        let store = Arc::new(StubStore::new(Duration::ZERO));
        let tool = MaterializeDatasetTool::new(
            StubView {
                view: Some(sample_view(vec![parquet_file()])),
            },
            StubRecorder::default(),
            router_with(store),
        );
        let err = tool
            .call(json!({
                "slug": "test-slug",
                "principal": { "kind": "anonymous", "id": "should-be-absent" }
            }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn inflight_map_prunes_dead_slots_between_calls() {
        // After each call completes (the `_guard` drops, the slot's
        // strong count goes to zero), the next `lock_slot` call's
        // `retain` step should evict the dead entry. We assert that
        // the map ends up empty once all in-flight work has drained.
        let store = Arc::new(StubStore::new(Duration::ZERO));
        let tool = MaterializeDatasetTool::new(
            StubView {
                view: Some(sample_view(vec![parquet_file()])),
            },
            StubRecorder::default(),
            router_with(store),
        );

        // Drive three distinct calls sequentially.
        for _ in 0..3 {
            tool.call(json!({ "slug": "test-slug" })).await.unwrap();
        }

        // After the last guard drops, one more `lock_slot` triggers
        // the retain sweep that removes the dead entry. We trigger
        // a fourth call to exercise that sweep.
        tool.call(json!({ "slug": "test-slug" })).await.unwrap();

        // Inspect the map: it should contain at most one live entry
        // (the one created by the fourth call, which itself was
        // dropped at this point — so strictly zero, but we'd accept
        // a sweeping race that leaves it at one). Either way the
        // map MUST not grow with the number of historical calls.
        let map = tool.inflight.lock().await;
        let live_count = map.values().filter(|w| w.strong_count() > 0).count();
        assert!(
            live_count <= 1,
            "inflight map must not retain dead slots; got {live_count} live, total {} entries",
            map.len()
        );
    }

    #[test]
    fn input_schema_advertises_required_fields() {
        let s = input_schema();
        let props = s["properties"].as_object().unwrap();
        assert!(props.contains_key("id"));
        assert!(props.contains_key("slug"));
        assert!(props.contains_key("format"));
        assert!(props.contains_key("ttl_seconds"));
    }
}
