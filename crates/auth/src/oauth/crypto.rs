//! AES-256-GCM at-rest encryption for stored OAuth tokens.
//!
//! The KEK is a 32-byte env-supplied key. Each row generates its
//! own 12-byte GCM nonce on encryption and stores it alongside
//! the ciphertext (see `oauth_accounts.access_token_nonce`).
//!
//! Every encrypt + decrypt carries associated data (AAD) that
//! binds the ciphertext to its row identity — typically
//! `b"<provider>:<provider_user_id>"`. Without AAD an attacker
//! with DB write access could swap Alice's ciphertext+nonce
//! into Bob's row and a future decrypt would happily produce
//! Alice's access token (a token mix-up attack). The AAD is
//! GCM-authenticated alongside the ciphertext, so any cross-row
//! swap fails authentication.
//!
//! KEK rotation strategy: the env knob is a single key for v0.1.
//! When we eventually rotate, the per-row wrapped-key pattern
//! lands as a schema migration that adds a `kek_id` column;
//! callers will read both `kek_id` + nonce and consult a
//! `HashMap<KekId, Kek>` — that's a v0.2 lift.

use aes_gcm::aead::{Aead, KeyInit, Payload};
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
    #[error("AES-GCM operation failed (corrupt ciphertext, wrong KEK, or AAD mismatch)")]
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

    /// Encrypt `plaintext` under a freshly-generated nonce, with
    /// `aad` GCM-authenticated alongside the ciphertext. Returns
    /// `(ciphertext, nonce)` for storage in the matching
    /// `*_ciphertext` + `*_nonce` columns.
    ///
    /// The AAD does not appear in the output; the caller must
    /// pass the same bytes to [`decrypt`] (typically derived
    /// from a row's stable identifying columns).
    pub fn encrypt(
        &self,
        plaintext: &[u8],
        aad: &[u8],
    ) -> Result<(Vec<u8>, [u8; NONCE_LEN]), TokenCipherError> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        OsRng
            .try_fill_bytes(&mut nonce_bytes)
            .expect("OsRng must provide entropy for GCM nonce");
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(
                nonce,
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| TokenCipherError::Crypto)?;
        Ok((ciphertext, nonce_bytes))
    }

    /// Decrypt a ciphertext using the supplied nonce + AAD. The
    /// KEK *and* the AAD must match the ones used at encrypt
    /// time; either mismatch fails the GCM tag and returns
    /// [`TokenCipherError::Crypto`] — that's how cross-row
    /// swap attacks fail.
    pub fn decrypt(
        &self,
        ciphertext: &[u8],
        nonce: &[u8],
        aad: &[u8],
    ) -> Result<Vec<u8>, TokenCipherError> {
        if nonce.len() != NONCE_LEN {
            return Err(TokenCipherError::BadNonceLength(nonce.len()));
        }
        let nonce = Nonce::from_slice(nonce);
        self.cipher
            .decrypt(
                nonce,
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .map_err(|_| TokenCipherError::Crypto)
    }
}

impl std::fmt::Debug for TokenCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The KEK is intentionally NOT printed.
        f.debug_struct("TokenCipher").finish_non_exhaustive()
    }
}

/// Canonical AAD for an `oauth_accounts` row.
///
/// Binding the ciphertext to `<provider>:<provider_user_id>`
/// means that even if an attacker rewrites a row's
/// `*_ciphertext` + `*_nonce` columns to copy values from
/// another row, the decrypt at use time will fail the GCM tag
/// because the AAD won't match — there's no way to "transplant"
/// a token without also knowing the KEK.
#[must_use]
pub fn account_aad(provider: &str, provider_user_id: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(provider.len() + provider_user_id.len() + 1);
    bytes.extend_from_slice(provider.as_bytes());
    bytes.push(b':');
    bytes.extend_from_slice(provider_user_id.as_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_kek() -> [u8; KEK_LEN] {
        [42u8; KEK_LEN]
    }

    fn aad() -> Vec<u8> {
        account_aad("github", "12345")
    }

    #[test]
    fn round_trip_recovers_plaintext() {
        let cipher = TokenCipher::new(&test_kek()).unwrap();
        let plaintext = b"gho_aaaabbbbccccddddeeeeffffgggghhhhiii".to_vec();
        let (ct, nonce) = cipher.encrypt(&plaintext, &aad()).unwrap();
        let back = cipher.decrypt(&ct, &nonce, &aad()).unwrap();
        assert_eq!(back, plaintext);
    }

    #[test]
    fn each_encrypt_uses_a_fresh_nonce() {
        let cipher = TokenCipher::new(&test_kek()).unwrap();
        let (_, n1) = cipher.encrypt(b"same", &aad()).unwrap();
        let (_, n2) = cipher.encrypt(b"same", &aad()).unwrap();
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
        let (ct, nonce) = a.encrypt(b"secret", &aad()).unwrap();
        let err = b.decrypt(&ct, &nonce, &aad()).unwrap_err();
        assert!(matches!(err, TokenCipherError::Crypto));
    }

    #[test]
    fn bad_nonce_length_is_rejected() {
        let cipher = TokenCipher::new(&test_kek()).unwrap();
        let err = cipher.decrypt(b"anything", &[0u8; 8], &aad()).unwrap_err();
        assert!(matches!(err, TokenCipherError::BadNonceLength(8)));
    }

    #[test]
    fn cross_row_swap_fails_authentication() {
        // Regression for Copilot R6: AES-GCM without AAD would
        // allow an attacker who can write to the DB to swap
        // Alice's ciphertext+nonce into Bob's row and have a
        // future decrypt succeed. With per-row AAD it doesn't.
        let cipher = TokenCipher::new(&test_kek()).unwrap();
        let alice_aad = account_aad("github", "alice-id");
        let bob_aad = account_aad("github", "bob-id");
        let (ct, nonce) = cipher.encrypt(b"alice-token", &alice_aad).unwrap();
        // Pretend we copied (ct, nonce) into Bob's row. The
        // decrypt under Bob's AAD must fail.
        let err = cipher.decrypt(&ct, &nonce, &bob_aad).unwrap_err();
        assert!(matches!(err, TokenCipherError::Crypto));
    }

    #[test]
    fn account_aad_is_stable_and_distinct() {
        assert_eq!(account_aad("github", "1"), account_aad("github", "1"));
        assert_ne!(account_aad("github", "1"), account_aad("github", "2"));
        assert_ne!(account_aad("github", "1"), account_aad("google", "1"));
    }
}
