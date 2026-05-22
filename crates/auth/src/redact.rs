//! PII-safe identifiers for logs + telemetry.
//!
//! Anywhere the crate would otherwise put a raw email address
//! into a `tracing` event, it goes through [`email`] instead.
//! The output is a short hex prefix of `sha256(addr)` — stable
//! enough for cross-line correlation in operator log queries,
//! short enough to fit comfortably in a tag, and lossy enough
//! that a log dump cannot enumerate registered addresses.

use sha2::{Digest, Sha256};

/// Length of the short identifier, in bytes (16 hex chars at 8 bytes).
const ID_BYTES: usize = 8;

/// Render `addr` as a short hex digest. Stable across calls with
/// the same input.
pub(crate) fn email(addr: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(addr.as_bytes());
    let digest = hasher.finalize();
    hex_lower(&digest[..ID_BYTES])
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(out, "{b:02x}").expect("writing to a String never fails");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_is_stable() {
        assert_eq!(email("alice@example.com"), email("alice@example.com"));
    }

    #[test]
    fn email_differs_for_distinct_inputs() {
        assert_ne!(email("alice@example.com"), email("bob@example.com"));
    }

    #[test]
    fn email_returns_16_hex_chars() {
        let id = email("anything");
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
