//! CSRF state token.
//!
//! 32-byte `OsRng` entropy, URL-safe + no-padding base64 (43
//! chars). Cleartext goes into the authorize-redirect query
//! string; only `sha256(state)` is persisted (in
//! `oauth_states.state_hash`) so a DB leak alone can't forge a
//! callback challenge.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::TryRngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};

pub const STATE_ENTROPY_BYTES: usize = 32;

/// A freshly-generated state token plus its lookup digest.
#[derive(Debug)]
pub struct StateToken {
    /// Goes in the authorize-redirect query string.
    pub cleartext: String,
    /// `sha256(cleartext)` — the form persisted in the DB.
    pub digest: Vec<u8>,
}

#[must_use]
pub fn generate_state() -> StateToken {
    let mut entropy = [0u8; STATE_ENTROPY_BYTES];
    OsRng
        .try_fill_bytes(&mut entropy)
        .expect("OsRng must provide entropy for CSRF state");
    let cleartext = URL_SAFE_NO_PAD.encode(entropy);
    let digest = hash_state(&cleartext);
    StateToken { cleartext, digest }
}

/// Compute the canonical lookup digest for a cleartext state
/// token. Stable across calls.
#[must_use]
pub fn hash_state(cleartext: &str) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(cleartext.as_bytes());
    hasher.finalize().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_digest_round_trips() {
        let s = generate_state();
        assert_eq!(s.digest, hash_state(&s.cleartext));
        assert_eq!(s.digest.len(), 32);
    }

    #[test]
    fn two_states_are_distinct() {
        let a = generate_state();
        let b = generate_state();
        assert_ne!(a.cleartext, b.cleartext);
    }

    #[test]
    fn state_is_url_safe_no_pad_43_chars() {
        let s = generate_state();
        assert_eq!(s.cleartext.len(), 43);
        assert!(!s.cleartext.contains('='));
    }
}
