//! PKCE S256 (RFC 7636).
//!
//! `code_verifier` is 32 bytes of `OsRng` encoded URL-safe +
//! no-padding base64 — 43 ASCII characters, well within the
//! 43–128 length window the RFC mandates. `code_challenge` is
//! `base64url-no-pad(sha256(code_verifier))`.
//!
//! The verifier is the secret kept server-side; only the
//! challenge ever travels over the wire as part of the
//! authorize-redirect query string.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::TryRngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};

/// Length of the raw entropy (in bytes) consumed for each
/// verifier. 32 bytes ≈ 256 bits matches RFC 7636's
/// recommendation.
pub const PKCE_ENTROPY_BYTES: usize = 32;

/// The two halves of a PKCE pair.
#[derive(Debug)]
pub struct PkcePair {
    /// Server-side secret. Persist in `oauth_states.code_verifier`;
    /// send to the provider on the token-exchange POST.
    pub code_verifier: String,
    /// Client-side challenge. Goes in the authorize-redirect
    /// query string. `base64url-no-pad(sha256(verifier))`.
    pub code_challenge: String,
}

/// Mint a fresh PKCE pair using `OsRng`.
#[must_use]
pub fn generate_pkce() -> PkcePair {
    let mut entropy = [0u8; PKCE_ENTROPY_BYTES];
    OsRng
        .try_fill_bytes(&mut entropy)
        .expect("OsRng must provide entropy for PKCE");
    let code_verifier = URL_SAFE_NO_PAD.encode(entropy);
    let code_challenge = challenge_for(&code_verifier);
    PkcePair {
        code_verifier,
        code_challenge,
    }
}

/// Compute `code_challenge` for a given verifier. Exposed so a
/// test can verify a server-issued challenge round-trips.
#[must_use]
pub fn challenge_for(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_is_url_safe_no_pad_43_chars() {
        let pair = generate_pkce();
        assert_eq!(pair.code_verifier.len(), 43);
        assert!(!pair.code_verifier.contains('='));
        assert!(!pair.code_verifier.contains('+'));
        assert!(!pair.code_verifier.contains('/'));
    }

    #[test]
    fn challenge_matches_sha256_of_verifier() {
        let pair = generate_pkce();
        assert_eq!(challenge_for(&pair.code_verifier), pair.code_challenge);
        assert_eq!(pair.code_challenge.len(), 43); // sha256 → 32 bytes → base64url-no-pad → 43 chars
    }

    #[test]
    fn two_pairs_are_distinct() {
        let a = generate_pkce();
        let b = generate_pkce();
        assert_ne!(a.code_verifier, b.code_verifier);
        assert_ne!(a.code_challenge, b.code_challenge);
    }

    #[test]
    fn challenge_is_stable_for_given_verifier() {
        let v = "constant_test_verifier_for_the_purposes_of_test";
        assert_eq!(challenge_for(v), challenge_for(v));
    }
}
