//! AES-256-GCM at-rest encryption for stored OAuth tokens.
//!
//! The KEK is a 32-byte env-supplied key. Each row generates its
//! own 12-byte GCM nonce on encryption and stores it alongside
//! the ciphertext (see `oauth_accounts.access_token_nonce`).
//! GCM authenticates both the ciphertext and any associated-data
//! we feed; v0.1 uses no AAD (the row identity is implicit).
//!
//! KEK rotation strategy: the env knob is a single key for v0.1.
//! When we eventually rotate, the per-row wrapped-key pattern
//! lands as a schema migration that adds a `kek_id` column;
//! callers will read both `kek_id` + nonce and consult a
//! `HashMap<KekId, Kek>` — that's a v0.2 lift.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use rand::TryRngCore;
use rand::rngs::OsRng;
use thiserror::Error;

/// Length of the KEK, in bytes (AES-256).
pub const KEK_LEN: usize = 32;
/// Length of a GCM nonce, in bytes (per `aes_gcm` defaults).
pub const NONCE_LEN: usize = 12;

/// AES-256-GCM cipher built once from a 32-byte KEK and reused
/// across encrypt + decrypt calls. Cheap to clone (internally
/// keeps an `Arc`-style handle).
#[derive(Clone)]
pub struct TokenCipher {
    cipher: Aes256Gcm,
}

#[derive(Debug, Error)]
pub enum TokenCipherError {
    #[error("OAUTH_TOKEN_KEK must be exactly {KEK_LEN} bytes, got {0}")]
    BadKekLength(usize),
    #[error("ciphertext nonce must be exactly {NONCE_LEN} bytes, got {0}")]
    BadNonceLength(usize),
    #[error("AES-GCM operation failed (likely a corrupt ciphertext or wrong KEK)")]
    Crypto,
}

impl TokenCipher {
    /// Build a cipher from a 32-byte KEK. Returns
    /// [`TokenCipherError::BadKekLength`] if the slice isn't
    /// 32 bytes — the env-decode path uses that to surface a
    /// helpful error at boot.
    pub fn new(kek_bytes: &[u8]) -> Result<Self, TokenCipherError> {
        if kek_bytes.len() != KEK_LEN {
            return Err(TokenCipherError::BadKekLength(kek_bytes.len()));
        }
        let key = Key::<Aes256Gcm>::from_slice(kek_bytes);
        Ok(Self {
            cipher: Aes256Gcm::new(key),
        })
    }

    /// Encrypt `plaintext` under a freshly-generated nonce.
    /// Returns `(ciphertext, nonce)` for storage in the matching
    /// `*_ciphertext` + `*_nonce` columns.
    pub fn encrypt(
        &self,
        plaintext: &[u8],
    ) -> Result<(Vec<u8>, [u8; NONCE_LEN]), TokenCipherError> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        OsRng
            .try_fill_bytes(&mut nonce_bytes)
            .expect("OsRng must provide entropy for GCM nonce");
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext)
            .map_err(|_| TokenCipherError::Crypto)?;
        Ok((ciphertext, nonce_bytes))
    }

    /// Decrypt a ciphertext using the supplied nonce. The KEK
    /// must match the one used at encrypt time, otherwise the
    /// GCM tag fails and the call returns
    /// [`TokenCipherError::Crypto`].
    pub fn decrypt(&self, ciphertext: &[u8], nonce: &[u8]) -> Result<Vec<u8>, TokenCipherError> {
        if nonce.len() != NONCE_LEN {
            return Err(TokenCipherError::BadNonceLength(nonce.len()));
        }
        let nonce = Nonce::from_slice(nonce);
        self.cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| TokenCipherError::Crypto)
    }
}

impl std::fmt::Debug for TokenCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The KEK is intentionally NOT printed.
        f.debug_struct("TokenCipher").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_kek() -> [u8; KEK_LEN] {
        [42u8; KEK_LEN]
    }

    #[test]
    fn round_trip_recovers_plaintext() {
        let cipher = TokenCipher::new(&test_kek()).unwrap();
        let plaintext = b"gho_aaaabbbbccccddddeeeeffffgggghhhhiii".to_vec();
        let (ct, nonce) = cipher.encrypt(&plaintext).unwrap();
        let back = cipher.decrypt(&ct, &nonce).unwrap();
        assert_eq!(back, plaintext);
    }

    #[test]
    fn each_encrypt_uses_a_fresh_nonce() {
        let cipher = TokenCipher::new(&test_kek()).unwrap();
        let (_, n1) = cipher.encrypt(b"same").unwrap();
        let (_, n2) = cipher.encrypt(b"same").unwrap();
        assert_ne!(n1, n2);
    }

    #[test]
    fn bad_kek_length_is_rejected_with_specific_error() {
        let err = TokenCipher::new(&[0u8; 16]).unwrap_err();
        assert!(matches!(err, TokenCipherError::BadKekLength(16)));
    }

    #[test]
    fn wrong_kek_fails_with_crypto_error() {
        let a = TokenCipher::new(&test_kek()).unwrap();
        let mut other_kek = test_kek();
        other_kek[0] = 0;
        let b = TokenCipher::new(&other_kek).unwrap();
        let (ct, nonce) = a.encrypt(b"secret").unwrap();
        let err = b.decrypt(&ct, &nonce).unwrap_err();
        assert!(matches!(err, TokenCipherError::Crypto));
    }

    #[test]
    fn bad_nonce_length_is_rejected() {
        let cipher = TokenCipher::new(&test_kek()).unwrap();
        let err = cipher.decrypt(b"anything", &[0u8; 8]).unwrap_err();
        assert!(matches!(err, TokenCipherError::BadNonceLength(8)));
    }
}
