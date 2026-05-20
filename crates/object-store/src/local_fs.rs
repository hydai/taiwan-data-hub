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
//! - `path` is restricted to a narrow allowlist
//!   (`A-Za-z 0-9 / . _ - ~`) so it round-trips through
//!   `Url` parsing byte-for-byte. We avoid the percent-encoding
//!   trap where the signed form and the URL-extracted form drift:
//!   if the input contained a space, `Url::join` would emit `%20`
//!   in the URL but the MAC would still be over the raw space —
//!   verification then mismatches. Rejecting at sign time
//!   surfaces the error early instead of producing a URL that's
//!   silently broken on the verify path.
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
        // Origin-only: a `base_url` with a path / query / fragment
        // would interact unintuitively with `Url::join` later (the
        // `files/dl/...` segment would be appended relative to the
        // configured path rather than the origin). Reject up front
        // so a misconfigured `OBJECT_STORE_BASE_URL` fails at boot
        // instead of producing silently-broken signed URLs.
        let path = base_url.path();
        if !(path.is_empty() || path == "/")
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(ObjectStoreError::InvalidUri(format!(
                "base_url must be an origin (no path/query/fragment), got {base_url}"
            )));
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

        // Restrict to characters that round-trip through `Url`
        // parsing without re-encoding. Anything else (spaces, `?`,
        // `#`, control chars, non-ASCII) would create a drift
        // between the signed bytes and the bytes the gateway
        // extracts from the URL, silently breaking verification.
        if !trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b'~'))
        {
            return Err(ObjectStoreError::InvalidUri(
                "file URI may only contain [A-Za-z0-9/._-~]; \
                 anything else would force URL-side re-encoding"
                    .to_owned(),
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
/// store's secret. Returns `Ok(())` when the signature is valid and
/// the URL has not yet expired; the gateway then resolves `path`
/// against its cache root itself. (We don't return `path` back —
/// the caller already has it from the request, and giving it back
/// would imply some kind of canonicalisation step we don't do here.)
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

    /// `base_url` must be an origin: any path / query / fragment
    /// would interact badly with `Url::join` later, producing
    /// surprising signed URLs. The construction-time check fails
    /// fast on misconfiguration.
    #[tokio::test]
    async fn rejects_non_origin_base_url() {
        for bad in [
            "https://hub.example.com/api",
            "https://hub.example.com/api/",
            "https://hub.example.com/?foo=1",
            "https://hub.example.com/#frag",
        ] {
            let err = LocalFsObjectStore::new(Url::parse(bad).unwrap(), TEST_SECRET.to_vec())
                .expect_err("non-origin base_url must be rejected");
            assert!(
                matches!(err, ObjectStoreError::InvalidUri(_)),
                "expected InvalidUri for {bad:?}, got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn accepts_bare_origin_with_or_without_trailing_slash() {
        for ok in [
            "https://hub.example.com",
            "https://hub.example.com/",
            "http://localhost:8080",
            "http://localhost:8080/",
        ] {
            LocalFsObjectStore::new(Url::parse(ok).unwrap(), TEST_SECRET.to_vec())
                .expect("origin (with or without trailing slash) accepted");
        }
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

    /// Defense for the MAC-encoding drift bug: characters that
    /// would force `Url` to percent-encode produce signatures that
    /// won't verify on the gateway side. Reject them at sign time
    /// so the failure is loud and immediate.
    #[tokio::test]
    async fn rejects_path_with_url_encoded_characters() {
        let store = store();
        for bad in [
            "file:///cache/has space.parquet",
            "file:///cache/has?query.parquet",
            "file:///cache/has#frag.parquet",
            "file:///cache/中文.parquet",
        ] {
            let err = store
                .presign_get(bad, Duration::from_secs(3600))
                .await
                .unwrap_err();
            assert!(
                matches!(err, ObjectStoreError::InvalidUri(_)),
                "expected InvalidUri for {bad:?}, got {err:?}"
            );
        }
    }

    /// Pin the round-trip for the full URL-encoded charset we DO
    /// allow. Slashes, dots, dashes, underscores, tildes — exactly
    /// the set `Url::join` leaves untouched.
    #[tokio::test]
    async fn signs_paths_with_full_allowed_charset() {
        let store = store();
        let signed = store
            .presign_get(
                "file:///cache/data_gov-tw/v1.0/foo.bar~baz.parquet",
                Duration::from_secs(3600),
            )
            .await
            .expect("allowed charset must sign");
        let url = Url::parse(&signed.url).unwrap();
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
        let expires: i64 = pairs.get("expires").unwrap().parse().unwrap();
        let sig = pairs.get("sig").unwrap();
        // The path the gateway will extract MUST verify against the
        // path we signed — proving no encoding drift.
        verify_signed_path(
            TEST_SECRET,
            url.path().trim_start_matches("/files/dl/"),
            expires,
            sig,
            fixed_now(),
        )
        .expect("URL round-trip MUST verify");
    }
}
