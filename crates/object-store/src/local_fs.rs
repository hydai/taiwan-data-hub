//! Local-filesystem backend that signs a download URL with HMAC.
//!
//! The backend doesn't actually serve the file — the gateway exposes
//! a `/files/dl/{path}?expires=…&sig=…` route (out of scope for this
//! crate; see #1.8 follow-up) that calls [`verify_signed_path`] and
//! streams the file from disk. This separation keeps the signing
//! logic and the HTTP serving in different crates.
//!
//! ## Wire format
//!
//! ```text
//! {base_url}/files/dl/{path}?expires={unix_secs}&sig={hex}
//!
//! sig = HMAC-SHA256(secret, "{path}\n{expires}")
//! ```
//!
//! Why this shape:
//!
//! - `path` is URL-percent-encoded *as a whole path* (slashes left
//!   intact so the route's path-extractor still works).
//! - The signed body is `{path}\n{expires}` — newline delimited so a
//!   path containing `expires=…` text can't ambiguate the MAC input.
//! - The signature is fixed-width hex (lowercase) for easy
//!   constant-time comparison in [`verify_signed_path`].

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use url::Url;

use crate::{ObjectStore, ObjectStoreError, PresignedUrl};

type HmacSha256 = Hmac<Sha256>;

/// Hard cap on `LocalFs` URL lifetime. Generous compared to S3
/// presigned URLs because operators running personal mode often want
/// a single URL to outlive a tab refresh; tightening is straight-
/// forward via [`LocalFsObjectStore::with_max_ttl`].
pub const DEFAULT_MAX_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Signs file:// URIs into HMAC-protected download URLs served by the
/// gateway. The signing secret is held inside the store, never
/// serialised, never logged.
#[derive(Clone)]
pub struct LocalFsObjectStore {
    base_url: Url,
    secret: Vec<u8>,
    max_ttl: Duration,
    /// Hook for tests: returns "now" so deterministic fixtures don't
    /// race the wall clock. Production uses [`Utc::now`].
    now_fn: fn() -> DateTime<Utc>,
}

impl std::fmt::Debug for LocalFsObjectStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Secret never lands in logs / panic output — only the base
        // URL is safe to surface.
        f.debug_struct("LocalFsObjectStore")
            .field("base_url", &self.base_url.as_str())
            .field("max_ttl_secs", &self.max_ttl.as_secs())
            .finish_non_exhaustive()
    }
}

impl LocalFsObjectStore {
    /// Build a store. `base_url` is the gateway's public origin
    /// (`http://127.0.0.1:8080`, `https://hub.example.com`, …);
    /// signed URLs are produced as `{base_url}/files/dl/...`.
    /// `secret` MUST be at least 32 bytes; shorter secrets are
    /// rejected to avoid accidentally signing with placeholder
    /// values.
    pub fn new(base_url: Url, secret: Vec<u8>) -> Result<Self, ObjectStoreError> {
        if secret.len() < 32 {
            return Err(ObjectStoreError::SigningFailed(
                "LocalFs signing secret must be at least 32 bytes".to_owned(),
            ));
        }
        Ok(Self {
            base_url,
            secret,
            max_ttl: DEFAULT_MAX_TTL,
            now_fn: Utc::now,
        })
    }

    /// Lower the maximum TTL (default 24h). Callers MAY raise it back
    /// to 7 days; AWS-compatible cap stays out of `LocalFs` since it
    /// has nothing to do with `SigV4`.
    pub fn with_max_ttl(mut self, max_ttl: Duration) -> Self {
        self.max_ttl = max_ttl;
        self
    }

    #[cfg(test)]
    fn with_now_fn(mut self, now_fn: fn() -> DateTime<Utc>) -> Self {
        self.now_fn = now_fn;
        self
    }

    fn now(&self) -> DateTime<Utc> {
        (self.now_fn)()
    }
}

#[async_trait]
impl ObjectStore for LocalFsObjectStore {
    async fn presign_get(
        &self,
        uri: &str,
        ttl: Duration,
    ) -> Result<PresignedUrl, ObjectStoreError> {
        if ttl > self.max_ttl {
            return Err(ObjectStoreError::TtlOutOfRange {
                requested: ttl,
                max: self.max_ttl,
            });
        }

        // Accept both `file://` URIs and bare paths so a caller doesn't
        // have to remember which one the storage layer used.
        let raw_path = uri.strip_prefix("file://").unwrap_or(uri);
        let trimmed = raw_path.trim_start_matches('/');
        if trimmed.is_empty() {
            return Err(ObjectStoreError::InvalidUri(
                "file URI is empty after stripping scheme".to_owned(),
            ));
        }
        // Reject `..` and absolute paths to defend the serving route
        // from escaping its base directory. The route does its own
        // canonical-path check too — defense in depth.
        let pathbuf = PathBuf::from(trimmed);
        if pathbuf
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(ObjectStoreError::InvalidUri(
                "file URI must not contain `..`".to_owned(),
            ));
        }

        let expires_at = self.now()
            + chrono::Duration::from_std(ttl).map_err(|e| {
                ObjectStoreError::TtlOutOfRange {
                    requested: ttl,
                    max: self.max_ttl,
                }
                .tap_log(|| format!("ttl conversion failed: {e}"))
            })?;
        let expires_secs = expires_at.timestamp();

        let signature = sign(&self.secret, trimmed, expires_secs);

        let mut url = self
            .base_url
            .join(&format!("files/dl/{trimmed}"))
            .map_err(|e| ObjectStoreError::InvalidUri(format!("join failed: {e}")))?;
        url.query_pairs_mut()
            .append_pair("expires", &expires_secs.to_string())
            .append_pair("sig", &signature);

        Ok(PresignedUrl {
            url: url.into(),
            expires_at,
        })
    }
}

/// Verify a `/files/dl/{path}?expires=…&sig=…` request against the
/// store's secret. Returns the canonical path string on success so
/// the gateway can resolve it against the cache root.
///
/// Constant-time comparison via `hmac::Mac::verify_slice` defends
/// against timing oracles.
pub fn verify_signed_path(
    secret: &[u8],
    path: &str,
    expires_secs: i64,
    sig_hex: &str,
    now: DateTime<Utc>,
) -> Result<(), VerifyError> {
    let expires_at = Utc
        .timestamp_opt(expires_secs, 0)
        .single()
        .ok_or(VerifyError::BadExpires)?;
    if expires_at <= now {
        return Err(VerifyError::Expired);
    }
    let sig_bytes = hex::decode(sig_hex).map_err(|_| VerifyError::BadSignature)?;
    let mut mac = HmacSha256::new_from_slice(secret).map_err(|_| VerifyError::BadKey)?;
    mac.update(path.as_bytes());
    mac.update(b"\n");
    mac.update(expires_secs.to_string().as_bytes());
    mac.verify_slice(&sig_bytes)
        .map_err(|_| VerifyError::BadSignature)
}

/// Verification failure modes. The gateway maps these to 4xx codes —
/// we deliberately don't distinguish "expired" from "bad signature"
/// in the externally-rendered error to avoid leaking which check
/// failed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VerifyError {
    #[error("signed URL has expired")]
    Expired,
    #[error("signature did not verify")]
    BadSignature,
    #[error("`expires` value is not a valid unix timestamp")]
    BadExpires,
    #[error("signing key is not usable")]
    BadKey,
}

fn sign(secret: &[u8], path: &str, expires_secs: i64) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(path.as_bytes());
    mac.update(b"\n");
    mac.update(expires_secs.to_string().as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Convenience to fold a tracing side effect into an error pipeline
/// without breaking the `?` chain. Used by [`LocalFsObjectStore::
/// presign_get`] to log diagnostics that don't change the caller-
/// facing error message.
trait TapLog {
    fn tap_log<F: FnOnce() -> String>(self, msg: F) -> Self;
}
impl<T> TapLog for T {
    fn tap_log<F: FnOnce() -> String>(self, msg: F) -> Self {
        tracing::warn!(detail = %msg(), "object_store internal");
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SECRET: &[u8; 32] = b"0123456789abcdef0123456789abcdef";

    fn fixed_now() -> DateTime<Utc> {
        // 2026-05-20T00:00:00Z — same as today's session date,
        // chosen so the rendered signatures are stable across runs.
        Utc.timestamp_opt(1_779_235_200, 0).single().unwrap()
    }

    fn store() -> LocalFsObjectStore {
        LocalFsObjectStore::new(
            Url::parse("https://hub.example.com").unwrap(),
            TEST_SECRET.to_vec(),
        )
        .unwrap()
        .with_now_fn(fixed_now)
    }

    #[tokio::test]
    async fn signs_file_uri_into_expected_shape() {
        let signed = store()
            .presign_get("file:///cache/foo.parquet", Duration::from_secs(3600))
            .await
            .unwrap();
        let url = Url::parse(&signed.url).unwrap();
        assert_eq!(url.host_str(), Some("hub.example.com"));
        assert_eq!(url.path(), "/files/dl/cache/foo.parquet");
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
        // 2026-05-20T00:00:00Z + 3600s = 2026-05-20T01:00:00Z = 1_779_238_800
        assert_eq!(pairs.get("expires").unwrap().as_ref(), "1779238800");
        let sig = pairs.get("sig").unwrap();
        assert_eq!(sig.len(), 64, "hex-encoded SHA256 is 64 chars");
    }

    #[tokio::test]
    async fn round_trip_verifies() {
        let store = store();
        let signed = store
            .presign_get("file:///cache/foo.parquet", Duration::from_secs(3600))
            .await
            .unwrap();
        let url = Url::parse(&signed.url).unwrap();
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
        let expires: i64 = pairs.get("expires").unwrap().parse().unwrap();
        let sig = pairs.get("sig").unwrap();
        verify_signed_path(
            TEST_SECRET,
            url.path().trim_start_matches("/files/dl/"),
            expires,
            sig,
            fixed_now(),
        )
        .expect("freshly signed URL must verify against the same secret + clock");
    }

    #[tokio::test]
    async fn tampered_path_fails_verification() {
        let store = store();
        let signed = store
            .presign_get("file:///cache/foo.parquet", Duration::from_secs(3600))
            .await
            .unwrap();
        let url = Url::parse(&signed.url).unwrap();
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
        let expires: i64 = pairs.get("expires").unwrap().parse().unwrap();
        let sig = pairs.get("sig").unwrap();
        let result =
            verify_signed_path(TEST_SECRET, "cache/EVIL.parquet", expires, sig, fixed_now());
        assert_eq!(result, Err(VerifyError::BadSignature));
    }

    #[tokio::test]
    async fn expired_url_rejected_even_with_good_signature() {
        let store = store();
        let signed = store
            .presign_get("file:///cache/foo.parquet", Duration::from_secs(60))
            .await
            .unwrap();
        let url = Url::parse(&signed.url).unwrap();
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
        let expires: i64 = pairs.get("expires").unwrap().parse().unwrap();
        let sig = pairs.get("sig").unwrap();
        let future = fixed_now() + chrono::Duration::seconds(3600);
        let result = verify_signed_path(
            TEST_SECRET,
            url.path().trim_start_matches("/files/dl/"),
            expires,
            sig,
            future,
        );
        assert_eq!(result, Err(VerifyError::Expired));
    }

    #[tokio::test]
    async fn rejects_parent_dir_traversal() {
        let store = store();
        let err = store
            .presign_get("file:///cache/../etc/passwd", Duration::from_secs(60))
            .await
            .unwrap_err();
        assert!(matches!(err, ObjectStoreError::InvalidUri(_)));
    }

    #[tokio::test]
    async fn rejects_short_secret() {
        let err =
            LocalFsObjectStore::new(Url::parse("https://hub").unwrap(), b"too short".to_vec())
                .unwrap_err();
        assert!(matches!(err, ObjectStoreError::SigningFailed(_)));
    }

    #[tokio::test]
    async fn rejects_overlong_ttl() {
        let store = store();
        let err = store
            .presign_get("file:///cache/foo.parquet", Duration::from_secs(48 * 3600))
            .await
            .unwrap_err();
        assert!(matches!(err, ObjectStoreError::TtlOutOfRange { .. }));
    }
}
