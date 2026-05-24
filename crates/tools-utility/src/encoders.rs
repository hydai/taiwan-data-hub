//! Pure encoding helpers shared by the `encoder_*` MCP tools.
//!
//! Each function returns a `Result<String, String>` — Ok with the
//! encoded/decoded text, Err with a human-readable reason the input
//! was rejected (the tool wrapper surfaces it as
//! `InvalidArguments`).

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE as BASE64_URL_SAFE};

pub fn base64_encode(input: &str, url_safe: bool) -> String {
    let bytes = input.as_bytes();
    if url_safe {
        BASE64_URL_SAFE.encode(bytes)
    } else {
        BASE64_STANDARD.encode(bytes)
    }
}

pub fn base64_decode(input: &str, url_safe: bool) -> Result<String, String> {
    let bytes = if url_safe {
        BASE64_URL_SAFE.decode(input).map_err(|e| e.to_string())?
    } else {
        BASE64_STANDARD.decode(input).map_err(|e| e.to_string())?
    };
    String::from_utf8(bytes).map_err(|_| "decoded bytes are not valid UTF-8".into())
}

/// `percent-encode` the input for use as a URL query-component
/// value: every byte except ASCII letters, digits, `-`, `_`, `.`,
/// and `~` is `%XX`-escaped. Matches what
/// `encodeURIComponent` does in JavaScript / what most server-
/// side query libs expect.
pub fn url_component_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            use std::fmt::Write as _;
            // `write!(&mut String, ...)` is the clippy-preferred form
            // over `push_str(&format!(...))` — it avoids the
            // intermediate allocation. `write!` to a String is
            // infallible so the `_` discard is fine.
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

pub fn url_component_decode(input: &str) -> Result<String, String> {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                if i + 2 >= bytes.len() {
                    return Err(format!("incomplete percent-escape at byte {i}"));
                }
                let high = hex_nibble(bytes[i + 1]).ok_or_else(|| {
                    format!(
                        "invalid hex digit {:?} at byte {}",
                        bytes[i + 1] as char,
                        i + 1
                    )
                })?;
                let low = hex_nibble(bytes[i + 2]).ok_or_else(|| {
                    format!(
                        "invalid hex digit {:?} at byte {}",
                        bytes[i + 2] as char,
                        i + 2
                    )
                })?;
                out.push((high << 4) | low);
                i += 3;
            }
            // Form-style `+` for space — handle it because callers
            // commonly mix application/x-www-form-urlencoded with
            // percent-encoded. Strict spec says + means space only
            // in form encoding; we accept it to match Node /
            // browser behaviour.
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| "decoded bytes are not valid UTF-8".into())
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub fn hex_encode(input: &str) -> String {
    hex::encode(input.as_bytes())
}

pub fn hex_decode(input: &str) -> Result<String, String> {
    let bytes = hex::decode(input).map_err(|e| e.to_string())?;
    String::from_utf8(bytes).map_err(|_| "decoded bytes are not valid UTF-8".into())
}

/// Decode a JWT into its three parts (header, payload, signature)
/// without verifying the signature. Useful for inspecting tokens
/// during debugging / pen-testing; do NOT use this for auth
/// decisions — always verify the signature with the appropriate
/// key before trusting the claims.
pub fn jwt_decode_unverified(token: &str) -> Result<JwtParts, String> {
    let mut parts = token.split('.');
    let header_b64 = parts.next().ok_or("missing header segment")?;
    let payload_b64 = parts.next().ok_or("missing payload segment")?;
    let signature_b64 = parts.next().ok_or("missing signature segment")?;
    if parts.next().is_some() {
        return Err("JWT must have exactly three dot-separated segments".into());
    }
    let header = decode_jwt_segment(header_b64)?;
    let payload = decode_jwt_segment(payload_b64)?;
    Ok(JwtParts {
        header,
        payload,
        signature_present: !signature_b64.is_empty(),
    })
}

fn decode_jwt_segment(segment: &str) -> Result<serde_json::Value, String> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let bytes = URL_SAFE_NO_PAD
        .decode(segment)
        .map_err(|e| format!("base64-url decode failed: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("segment is not JSON: {e}"))
}

#[derive(Debug, Clone)]
pub struct JwtParts {
    pub header: serde_json::Value,
    pub payload: serde_json::Value,
    pub signature_present: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_standard_round_trip() {
        let encoded = base64_encode("Hello, World!", false);
        assert_eq!(encoded, "SGVsbG8sIFdvcmxkIQ==");
        assert_eq!(base64_decode(&encoded, false).unwrap(), "Hello, World!");
    }

    #[test]
    fn base64_url_safe_round_trip() {
        let input = "?>=<.,;:/!@#$%^&*()";
        let encoded = base64_encode(input, true);
        // URL-safe alphabet excludes `+` and `/`.
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        assert_eq!(base64_decode(&encoded, true).unwrap(), input);
    }

    #[test]
    fn base64_decode_rejects_invalid() {
        assert!(base64_decode("not base 64!!!", false).is_err());
    }

    #[test]
    fn url_encode_basic() {
        assert_eq!(url_component_encode("hello world"), "hello%20world");
        assert_eq!(url_component_encode("a=b&c=d"), "a%3Db%26c%3Dd");
        assert_eq!(url_component_encode("safe-chars._~"), "safe-chars._~");
    }

    #[test]
    fn url_decode_basic() {
        assert_eq!(
            url_component_decode("hello%20world").unwrap(),
            "hello world"
        );
        assert_eq!(url_component_decode("a%3Db%26c%3Dd").unwrap(), "a=b&c=d");
        // `+` → space (form-encoding tolerance)
        assert_eq!(url_component_decode("hello+world").unwrap(), "hello world");
    }

    #[test]
    fn url_decode_rejects_truncated_escape() {
        assert!(url_component_decode("hello%2").is_err());
    }

    #[test]
    fn hex_round_trip() {
        let encoded = hex_encode("abc");
        assert_eq!(encoded, "616263");
        assert_eq!(hex_decode(&encoded).unwrap(), "abc");
    }

    /// A real-looking JWT (no real signature). Headers/payload from
    /// jwt.io's playground; we just verify decoding works.
    #[test]
    fn jwt_decode_typical() {
        let token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let parts = jwt_decode_unverified(token).unwrap();
        assert_eq!(parts.header["alg"], "HS256");
        assert_eq!(parts.payload["sub"], "1234567890");
        assert!(parts.signature_present);
    }

    #[test]
    fn jwt_decode_rejects_two_segments() {
        assert!(jwt_decode_unverified("a.b").is_err());
    }
}
