//! S3-compatible backend: hand-rolled `SigV4` `GetObject` presigning.
//!
//! Works against any server that speaks the S3 wire protocol:
//! **`SeaweedFS`** (`DESIGN.md`'s target), Garage, Ceph RGW, `MinIO`
//! bridge, real AWS S3. We avoid the `aws-sdk-s3` dep on purpose —
//! the SDK is ~30 transitive crates and a multi-minute first build,
//! and we only need `GetObject` presigning. `SigV4` `GetObject` is a
//! deterministic ~80-line transform that has been stable since 2012;
//! the upside of a small surface area outweighs the SDK's broader
//! coverage we don't use.
//!
//! ## Spec references
//!
//! - AWS general reference: "Signature Version 4 Signing Process"
//! - "Authenticating Requests: Using Query Parameters (AWS Signature
//!   Version 4)" — the canonical recipe for presigned URLs.
//!
//! ## Security
//!
//! - Credentials are stored as bytes; the secret never lands in any
//!   `Debug` / `Display` impl.
//! - The whole signing pipeline takes the supplied `now()` as a
//!   parameter (via the [`S3ObjectStore`] hook) so test fixtures
//!   produce byte-stable signatures without racing the wall clock.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use url::Url;

use crate::{ObjectStore, ObjectStoreError, PresignedUrl};

type HmacSha256 = Hmac<Sha256>;

/// AWS-mandated cap on presigned URL lifetime.
const MAX_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Service name in the `SigV4` credential scope. Constant for S3.
const SERVICE: &str = "s3";
/// `UNSIGNED-PAYLOAD` is the documented payload hash for presigned
/// GET URLs (the body is empty, but the recipe still requires the
/// special sentinel to land in `x-amz-content-sha256`).
const PAYLOAD_SENTINEL: &str = "UNSIGNED-PAYLOAD";

/// Minimal credential set. `session_token` is `Some` only for STS-
/// minted credentials; `SeaweedFS` / static deployments leave it
/// `None`.
#[derive(Clone)]
pub struct S3Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

impl std::fmt::Debug for S3Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Credentials")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"REDACTED")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "REDACTED"),
            )
            .finish()
    }
}

/// Presigning store. Clone is cheap (credentials and config are
/// behind owned `String`s in the struct, all small).
#[derive(Clone, Debug)]
pub struct S3ObjectStore {
    endpoint: Url,
    region: String,
    credentials: S3Credentials,
    /// Path-style (e.g. `SeaweedFS`, `MinIO` default) vs. virtual-hosted
    /// (real AWS, where the bucket lands in the host). Path-style is
    /// the safe default for self-hosted setups.
    path_style: bool,
    now_fn: fn() -> DateTime<Utc>,
}

impl S3ObjectStore {
    /// Build a store. `endpoint` is the S3 service URL
    /// (`http://seaweedfs:8333`, `https://s3.eu-west-1.amazonaws.com`);
    /// `region` is the `SigV4` region label (`"us-east-1"` is the safe
    /// default for `SeaweedFS`).
    ///
    /// Returns [`ObjectStoreError::InvalidUri`] when the endpoint
    /// carries a non-empty path / query / fragment. `build_request_url`
    /// later overwrites the path with the `/{bucket}/{key}` form, so
    /// any configured prefix would silently disappear from the
    /// signature — surfacing the misconfiguration at construction is
    /// safer than producing presigned URLs that 404 against a
    /// reverse-proxied deployment.
    pub fn new(
        endpoint: Url,
        region: String,
        credentials: S3Credentials,
    ) -> Result<Self, ObjectStoreError> {
        let path = endpoint.path();
        if !(path.is_empty() || path == "/")
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(ObjectStoreError::InvalidUri(format!(
                "S3 endpoint must be an origin (no path/query/fragment), got {endpoint}"
            )));
        }
        Ok(Self {
            endpoint,
            region,
            credentials,
            path_style: true,
            now_fn: Utc::now,
        })
    }

    /// Use virtual-hosted-style URLs (bucket in the host). Only set
    /// this for real AWS or backends that DNS-route bucket names.
    pub fn with_virtual_hosted_style(mut self) -> Self {
        self.path_style = false;
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

    /// Resolve `s3://bucket/key/path` into the request URL the
    /// signature is computed against. The query string is added by
    /// the caller in [`Self::presign_get`].
    fn build_request_url(&self, bucket: &str, key: &str) -> Result<Url, ObjectStoreError> {
        let key_path = uri_encode_path(key);
        let mut base = self.endpoint.clone();
        if self.path_style {
            let path = format!("/{bucket}/{key_path}");
            base.set_path(&path);
        } else {
            // Virtual-hosted: bucket becomes the leftmost host label.
            let host = base
                .host_str()
                .ok_or_else(|| ObjectStoreError::InvalidUri("endpoint has no host".to_owned()))?;
            let new_host = format!("{bucket}.{host}");
            base.set_host(Some(&new_host))
                .map_err(|e| ObjectStoreError::InvalidUri(format!("set_host failed: {e}")))?;
            base.set_path(&format!("/{key_path}"));
        }
        Ok(base)
    }
}

#[async_trait]
impl ObjectStore for S3ObjectStore {
    async fn presign_get(
        &self,
        uri: &str,
        ttl: Duration,
    ) -> Result<PresignedUrl, ObjectStoreError> {
        if ttl > MAX_TTL {
            return Err(ObjectStoreError::TtlOutOfRange {
                requested: ttl,
                max: MAX_TTL,
            });
        }
        let (bucket, key) = parse_s3_uri(uri)?;
        let request_url = self.build_request_url(bucket, key)?;

        let now = self.now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = now.format("%Y%m%d").to_string();
        let credential_scope = format!("{date_stamp}/{}/{SERVICE}/aws4_request", self.region);
        let credential = format!("{}/{credential_scope}", self.credentials.access_key_id);

        // The signed-headers list MUST be alphabetically sorted and
        // lower-cased. `host` is the only one we sign — adding more
        // would require they appear in the canonical request too.
        let signed_headers = "host";

        let host = host_with_port(&request_url);

        // Query parameters that go into BOTH the URL and the
        // canonical request, in alphabetical order by key.
        let mut query: Vec<(&str, String)> = vec![
            ("X-Amz-Algorithm", "AWS4-HMAC-SHA256".to_owned()),
            ("X-Amz-Credential", credential.clone()),
            ("X-Amz-Date", amz_date.clone()),
            ("X-Amz-Expires", ttl.as_secs().to_string()),
            ("X-Amz-SignedHeaders", signed_headers.to_owned()),
        ];
        if let Some(token) = self.credentials.session_token.as_deref() {
            query.push(("X-Amz-Security-Token", token.to_owned()));
        }
        // SigV4 spec: canonical-query is sorted by encoded-key,
        // tiebreak by encoded-value. Our keys are unique so a key-
        // only sort is sufficient.
        query.sort_by(|a, b| a.0.cmp(b.0));

        let canonical_query = query
            .iter()
            .map(|(k, v)| format!("{}={}", uri_encode_strict(k), uri_encode_strict(v)))
            .collect::<Vec<_>>()
            .join("&");

        let canonical_headers = format!("host:{host}\n");
        let canonical_path = request_url.path().to_owned();
        let canonical_request = format!(
            "GET\n{canonical_path}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{PAYLOAD_SENTINEL}"
        );

        let mut hasher = Sha256::new();
        hasher.update(canonical_request.as_bytes());
        let canonical_request_hash = hex::encode(hasher.finalize());

        let string_to_sign =
            format!("AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{canonical_request_hash}");

        let signing_key = derive_signing_key(
            &self.credentials.secret_access_key,
            &date_stamp,
            &self.region,
        )?;
        let signature = hmac_hex(&signing_key, string_to_sign.as_bytes())?;

        // Final URL: canonical query + signature appended.
        let mut final_url = request_url;
        final_url.set_query(Some(&format!(
            "{canonical_query}&X-Amz-Signature={signature}"
        )));

        let expires_at = now
            + chrono::Duration::from_std(ttl).map_err(|_| ObjectStoreError::TtlOutOfRange {
                requested: ttl,
                max: MAX_TTL,
            })?;
        Ok(PresignedUrl {
            url: final_url.into(),
            expires_at,
        })
    }
}

/// Parse `s3://bucket/key/with/slashes` into `(bucket, key)`.
fn parse_s3_uri(uri: &str) -> Result<(&str, &str), ObjectStoreError> {
    let rest = uri
        .strip_prefix("s3://")
        .ok_or_else(|| ObjectStoreError::InvalidUri(format!("not an s3:// URI: {uri}")))?;
    let (bucket, key) = rest
        .split_once('/')
        .ok_or_else(|| ObjectStoreError::InvalidUri("s3 URI missing key part".to_owned()))?;
    if bucket.is_empty() || key.is_empty() {
        return Err(ObjectStoreError::InvalidUri(
            "s3 URI bucket and key must both be non-empty".to_owned(),
        ));
    }
    Ok((bucket, key))
}

/// AWS-style URI encoding for path components: percent-encode every
/// byte except the unreserved set, **including** `/` for query
/// values but **excluding** `/` for path values. The spec calls this
/// out as the most common `SigV4` implementation bug.
fn uri_encode_path(path: &str) -> String {
    path.split('/')
        .map(uri_encode_strict)
        .collect::<Vec<_>>()
        .join("/")
}

/// Strict per-spec encoding: A-Z a-z 0-9 - _ . ~ are passthrough;
/// every other byte becomes `%XX` (uppercase hex).
fn uri_encode_strict(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(*byte as char);
        } else {
            use std::fmt::Write;
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

fn host_with_port(url: &Url) -> String {
    match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or("")),
        None => url.host_str().unwrap_or("").to_owned(),
    }
}

fn derive_signing_key(
    secret: &str,
    date_stamp: &str,
    region: &str,
) -> Result<Vec<u8>, ObjectStoreError> {
    let k_secret = format!("AWS4{secret}");
    let k_date = hmac_bytes(k_secret.as_bytes(), date_stamp.as_bytes())?;
    let k_region = hmac_bytes(&k_date, region.as_bytes())?;
    let k_service = hmac_bytes(&k_region, SERVICE.as_bytes())?;
    let k_signing = hmac_bytes(&k_service, b"aws4_request")?;
    Ok(k_signing)
}

fn hmac_bytes(key: &[u8], data: &[u8]) -> Result<Vec<u8>, ObjectStoreError> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|e| ObjectStoreError::SigningFailed(format!("HMAC key error: {e}")))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn hmac_hex(key: &[u8], data: &[u8]) -> Result<String, ObjectStoreError> {
    Ok(hex::encode(hmac_bytes(key, data)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixed_now() -> DateTime<Utc> {
        // 2026-05-20T12:00:00Z — gives byte-stable signatures.
        Utc.timestamp_opt(1_779_278_400, 0).single().unwrap()
    }

    fn creds() -> S3Credentials {
        S3Credentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_owned(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_owned(),
            session_token: None,
        }
    }

    #[tokio::test]
    async fn presigns_seaweedfs_style_url() {
        let store = S3ObjectStore::new(
            Url::parse("http://seaweedfs.local:8333").unwrap(),
            "us-east-1".to_owned(),
            creds(),
        )
        .unwrap()
        .with_now_fn(fixed_now);
        let signed = store
            .presign_get(
                "s3://cache/data_gov_tw/foo/1.parquet",
                Duration::from_secs(3600),
            )
            .await
            .unwrap();
        let url = Url::parse(&signed.url).unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("seaweedfs.local"));
        assert_eq!(url.port(), Some(8333));
        assert_eq!(url.path(), "/cache/data_gov_tw/foo/1.parquet");
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(
            pairs.get("X-Amz-Algorithm").unwrap().as_ref(),
            "AWS4-HMAC-SHA256"
        );
        assert!(
            pairs
                .get("X-Amz-Credential")
                .unwrap()
                .contains("AKIAIOSFODNN7EXAMPLE/20260520/us-east-1/s3/aws4_request")
        );
        assert_eq!(pairs.get("X-Amz-Expires").unwrap().as_ref(), "3600");
        let sig = pairs.get("X-Amz-Signature").unwrap();
        assert_eq!(sig.len(), 64);
    }

    #[tokio::test]
    async fn signature_is_byte_stable_for_fixed_inputs() {
        let store = S3ObjectStore::new(
            Url::parse("http://seaweedfs.local:8333").unwrap(),
            "us-east-1".to_owned(),
            creds(),
        )
        .unwrap()
        .with_now_fn(fixed_now);
        let signed_a = store
            .presign_get(
                "s3://cache/data_gov_tw/foo/1.parquet",
                Duration::from_secs(3600),
            )
            .await
            .unwrap();
        let signed_b = store
            .presign_get(
                "s3://cache/data_gov_tw/foo/1.parquet",
                Duration::from_secs(3600),
            )
            .await
            .unwrap();
        assert_eq!(signed_a.url, signed_b.url, "deterministic for same inputs");
    }

    #[tokio::test]
    async fn rejects_non_s3_uri() {
        let store = S3ObjectStore::new(
            Url::parse("http://seaweedfs.local:8333").unwrap(),
            "us-east-1".to_owned(),
            creds(),
        )
        .unwrap();
        let err = store
            .presign_get("file:///not/s3", Duration::from_secs(60))
            .await
            .unwrap_err();
        assert!(matches!(err, ObjectStoreError::InvalidUri(_)));
    }

    #[tokio::test]
    async fn rejects_ttl_beyond_aws_cap() {
        let store = S3ObjectStore::new(
            Url::parse("http://seaweedfs.local:8333").unwrap(),
            "us-east-1".to_owned(),
            creds(),
        )
        .unwrap();
        let err = store
            .presign_get("s3://b/k", Duration::from_secs(8 * 24 * 60 * 60))
            .await
            .unwrap_err();
        assert!(matches!(err, ObjectStoreError::TtlOutOfRange { .. }));
    }

    /// Pin the construction-time origin check. Any path / query /
    /// fragment on `endpoint` would silently disappear at signing
    /// time because `build_request_url` rewrites the path; rejecting
    /// at construction surfaces misconfiguration loudly.
    #[tokio::test]
    async fn rejects_endpoint_with_path_prefix() {
        for bad in [
            "https://proxy.example.com/s3",
            "https://proxy.example.com/s3/",
            "https://proxy.example.com/?foo=1",
            "https://proxy.example.com/#frag",
        ] {
            let err = S3ObjectStore::new(Url::parse(bad).unwrap(), "us-east-1".to_owned(), creds())
                .expect_err("non-origin endpoint must be rejected");
            assert!(
                matches!(err, ObjectStoreError::InvalidUri(_)),
                "expected InvalidUri for {bad:?}, got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn accepts_bare_origin_and_trailing_slash() {
        for ok in [
            "https://s3.amazonaws.com",
            "https://s3.amazonaws.com/",
            "http://seaweedfs.local:8333",
            "http://seaweedfs.local:8333/",
        ] {
            S3ObjectStore::new(Url::parse(ok).unwrap(), "us-east-1".to_owned(), creds())
                .expect("origin (with or without trailing slash) accepted");
        }
    }

    #[test]
    fn uri_encode_strict_matches_aws_test_vectors() {
        // Per AWS docs: only A-Za-z0-9-_.~ are passthrough.
        assert_eq!(uri_encode_strict("abc-_.~"), "abc-_.~");
        assert_eq!(uri_encode_strict(" "), "%20");
        assert_eq!(uri_encode_strict("="), "%3D");
        assert_eq!(uri_encode_strict("/"), "%2F");
        assert_eq!(uri_encode_strict("中"), "%E4%B8%AD");
    }

    #[test]
    fn uri_encode_path_keeps_slashes() {
        assert_eq!(uri_encode_path("a/b c/d"), "a/b%20c/d");
    }

    #[test]
    fn parse_s3_uri_rejects_malformed() {
        assert!(parse_s3_uri("not-an-s3-uri").is_err());
        assert!(parse_s3_uri("s3://only-bucket").is_err());
        assert!(parse_s3_uri("s3:///empty-bucket/key").is_err());
    }

    #[test]
    fn credentials_debug_redacts_secret() {
        let dbg = format!("{:?}", creds());
        assert!(
            !dbg.contains("wJalrXU"),
            "secret leaked in debug output: {dbg}"
        );
        assert!(dbg.contains("REDACTED"));
    }
}
