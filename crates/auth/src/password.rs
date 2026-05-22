//! Argon2id wrapper.
//!
//! We use the `argon2` crate's high-level PHC-string API so the
//! stored hash carries its own parameters (`$argon2id$v=19$m=…`).
//! That keeps the `users.password_hash` column self-describing and
//! lets us rotate parameters without a schema migration: a future
//! login that decodes an old hash can re-hash with the new
//! parameters in the same transaction.
//!
//! No `Password` newtype here — the bytes only live in two places
//! (the registration / reset call site and this module's input
//! parameter) and both immediately consume them. Adding a wrapper
//! would only hide that the secret is never stored.

use argon2::password_hash::{PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng};
use argon2::{Argon2, PasswordHash};

use crate::error::AuthError;

/// Hash a plaintext password with argon2id (PHC defaults). Returns
/// the encoded `$argon2id$…` string ready for the `users.password_hash`
/// column.
pub fn hash_password(plaintext: &str) -> Result<String, AuthError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(plaintext.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AuthError::PasswordHash(e.to_string()))
}

/// Verify a plaintext password against the stored PHC hash. Returns
/// `Ok(true)` on match, `Ok(false)` on mismatch, and only `Err` for
/// structurally-invalid hashes (a corrupt column or a hand-edit).
/// Callers map both `Ok(false)` and `Err` to
/// [`AuthError::InvalidCredentials`] so timing + response are
/// indistinguishable.
pub fn verify_password(plaintext: &str, hash: &str) -> Result<bool, AuthError> {
    let parsed = PasswordHash::new(hash).map_err(|e| AuthError::PasswordHash(e.to_string()))?;
    match Argon2::default().verify_password(plaintext.as_bytes(), &parsed) {
        Ok(()) => Ok(true),
        Err(argon2::password_hash::Error::Password) => Ok(false),
        Err(e) => Err(AuthError::PasswordHash(e.to_string())),
    }
}

/// A throwaway argon2id verify against a dummy hash, used by the
/// login path when the email doesn't exist. Runs at the same
/// approximate cost as a real verify so an attacker can't
/// distinguish "user unknown" from "user known, wrong password"
/// by timing.
///
/// The hash below was generated with the same default parameters
/// as `hash_password`; the salt is fixed so the constant is stable
/// across builds.
pub const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$ZHVtbXktZHVtbXktZHVtbQ$X3W7t4dDuTwd6PEvxqaCBNRYsovOdpsxIeUuCFNqu+w";

/// Run a verify against [`DUMMY_HASH`] purely for its timing
/// contribution. The boolean result is discarded.
pub fn verify_dummy(plaintext: &str) {
    let _ = verify_password(plaintext, DUMMY_HASH);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_then_verify_succeeds() {
        let hash = hash_password("correct-horse-battery-staple").unwrap();
        assert!(verify_password("correct-horse-battery-staple", &hash).unwrap());
    }

    #[test]
    fn verify_rejects_wrong_password() {
        let hash = hash_password("correct").unwrap();
        assert!(!verify_password("wrong", &hash).unwrap());
    }

    #[test]
    fn verify_errors_on_corrupt_hash() {
        let err = verify_password("anything", "not-a-phc-string").unwrap_err();
        match err {
            AuthError::PasswordHash(_) => {}
            other => panic!("expected PasswordHash, got {other:?}"),
        }
    }

    #[test]
    fn dummy_hash_parses_and_rejects_arbitrary_input() {
        // If DUMMY_HASH ever rots, the login path's timing-equalisation
        // would silently fall apart. This catches that at CI time.
        assert!(!verify_password("anything", DUMMY_HASH).unwrap());
    }

    #[test]
    fn hashes_are_unique_per_call_due_to_random_salt() {
        let a = hash_password("same").unwrap();
        let b = hash_password("same").unwrap();
        assert_ne!(
            a, b,
            "argon2 should produce different hashes for same input"
        );
    }
}
