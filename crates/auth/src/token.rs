//! Magic-link tokens.
//!
//! Every token has two forms: the URL-encoded cleartext the user
//! receives in their inbox, and the SHA-256 hash we persist. The
//! DB never holds the cleartext, so a column dump alone cannot
//! mint a working link. Tokens are 32 bytes of `OsRng`, encoded
//! URL-safe + no-padding so they survive `?token=…` round-trips
//! through email clients that re-wrap long URLs.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::TryRngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};

/// Length of the raw entropy, in bytes. 32 bytes ≈ 256 bits, well
/// past the birthday bound for any reasonable token volume.
pub const TOKEN_ENTROPY_BYTES: usize = 32;

/// Length of the SHA-256 digest stored in `auth_tokens.token_hash`.
/// Provided as a `const` so callers (esp. test fixtures) don't
/// have to import `sha2`.
pub const TOKEN_HASH_BYTES: usize = 32;

/// A freshly-generated magic-link token in both forms.
#[derive(Debug)]
pub struct GeneratedToken {
    /// The URL-safe string mailed to the user. Treat as a secret
    /// even in test logs.
    pub cleartext: String,
    /// `sha256(cleartext)` — the form persisted in the DB.
    pub digest: Vec<u8>,
}

/// Generate a fresh magic-link token. Returns both the cleartext
/// for the mail body and the hash for the DB row.
///
/// Panics only if `OsRng::try_fill_bytes` returns an error, which
/// the `getrandom` documentation reserves for "the OS entropy
/// source has failed catastrophically" — there is nothing useful
/// to do in that case.
#[must_use]
pub fn generate_token() -> GeneratedToken {
    let mut entropy = [0u8; TOKEN_ENTROPY_BYTES];
    OsRng
        .try_fill_bytes(&mut entropy)
        .expect("OsRng must provide entropy for auth tokens");
    let cleartext = URL_SAFE_NO_PAD.encode(entropy);
    let digest = sha256(cleartext.as_bytes());
    GeneratedToken { cleartext, digest }
}

/// Compute the canonical lookup digest for a cleartext token. The
/// service uses this to find the matching `auth_tokens` row at
/// redemption time.
#[must_use]
pub fn digest_token(cleartext: &str) -> Vec<u8> {
    sha256(cleartext.as_bytes())
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_token_digest_matches_lookup_digest() {
        let GeneratedToken { cleartext, digest } = generate_token();
        assert_eq!(digest, digest_token(&cleartext));
        assert_eq!(digest.len(), TOKEN_HASH_BYTES);
    }

    #[test]
    fn cleartext_is_url_safe_no_pad_base64() {
        let GeneratedToken { cleartext, .. } = generate_token();
        // Round-trip decode must succeed and yield the expected byte length.
        let bytes = URL_SAFE_NO_PAD.decode(&cleartext).expect("re-decodes");
        assert_eq!(bytes.len(), TOKEN_ENTROPY_BYTES);
        // No padding char + only URL-safe alphabet (`-` and `_` allowed).
        assert!(!cleartext.contains('='));
        assert!(!cleartext.contains('+'));
        assert!(!cleartext.contains('/'));
    }

    #[test]
    fn two_generated_tokens_are_distinct() {
        let a = generate_token();
        let b = generate_token();
        assert_ne!(a.cleartext, b.cleartext);
        assert_ne!(a.digest, b.digest);
    }

    #[test]
    fn digest_is_deterministic_for_given_input() {
        let cleartext = "constant";
        assert_eq!(digest_token(cleartext), digest_token(cleartext));
    }
}
