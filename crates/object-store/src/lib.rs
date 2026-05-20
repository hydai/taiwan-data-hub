//! Presigned-URL abstraction over file-system and S3-compatible
//! storage backends.
//!
//! `materialize_dataset` (#1.8) and any future tool that hands a
//! caller a time-limited download URL goes through this crate so the
//! gateway can swap backends — `LocalFsObjectStore` for personal mode
//! / tests, `S3ObjectStore` for `SeaweedFS` / Garage / `MinIO`-bridge
//! deploys — without the tool layer caring.
//!
//! ## Why two backends?
//!
//! `dataset_files.uri` is deliberately abstract (`DESIGN.md` §4.3):
//! `file://` for local cache, `s3://` for object-store cache,
//! `https://` for upstream passthrough. The MCP tool resolves the
//! scheme, picks the right [`ObjectStore`], and calls
//! [`ObjectStore::presign_get`]; callers receive a `PresignedUrl`
//! and never see the underlying credentials or path layout.
//!
//! ## Why not pull `aws-sdk-s3`?
//!
//! The full SDK is ~30 transitive crates and a multi-minute first
//! build. We only need `GetObject` presigning, which is a
//! deterministic `SigV4` transform — about 80 lines of stable code in
//! [`s3`]. The trade is documented there.

pub mod local_fs;
pub mod s3;

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;

pub use local_fs::{LocalFsObjectStore, verify_signed_path};
pub use s3::{S3Credentials, S3ObjectStore};

/// A presigned URL that grants short-lived read access to one object.
///
/// `url` is the full request URL the caller GETs; `expires_at` is the
/// wall-clock instant after which the URL stops working (set by the
/// signer, not extracted from the URL — backends without a `?expires`
/// query param wouldn't expose the value otherwise).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresignedUrl {
    pub url: String,
    pub expires_at: DateTime<Utc>,
}

/// Errors a backend can return when signing.
#[derive(Debug, Error)]
pub enum ObjectStoreError {
    /// The input URI didn't conform to what the backend expects
    /// (wrong scheme, missing key, malformed bucket name, …).
    #[error("invalid object URI: {0}")]
    InvalidUri(String),
    /// TTL was outside the backend's supported range. AWS `SigV4` caps
    /// presigned URL lifetime at 7 days; `LocalFs` is bounded by what
    /// we want operators to allow.
    #[error("ttl {requested:?} out of supported range (max {max:?})")]
    TtlOutOfRange { requested: Duration, max: Duration },
    /// Backend-specific signing failure. Used by [`s3`] when the HMAC
    /// chain cannot be constructed (e.g. invalid credential bytes);
    /// callers typically surface this as a 500-class error because
    /// it implies misconfiguration, not bad input.
    #[error("signing failed: {0}")]
    SigningFailed(String),
}

/// Object-safe presigned-URL backend. Implementations clone cheaply
/// (typically `Arc`-share state); the gateway constructs one per
/// scheme at boot and hands the trait object to the MCP tool layer.
#[async_trait]
pub trait ObjectStore: Send + Sync + 'static {
    /// Issue a GET-only presigned URL for `uri`. `uri` is the raw
    /// `dataset_files.uri` value — backends are responsible for
    /// parsing their own scheme and returning [`ObjectStoreError::
    /// InvalidUri`] when the scheme doesn't match.
    async fn presign_get(&self, uri: &str, ttl: Duration)
    -> Result<PresignedUrl, ObjectStoreError>;
}
