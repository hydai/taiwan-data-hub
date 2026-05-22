//! Shared sqlx → `StorageError` mappers.
//!
//! Centralises the SQLSTATE handling so individual repo modules
//! don't each reach into `sqlx::Error::Database` and re-implement
//! the same constraint-name extraction (and don't accidentally
//! diverge on whether they leak the full Postgres `detail` — which
//! can echo conflicting column values like an email address into
//! logs or error responses).

use crate::StorageError;

/// Map a `sqlx::Error` to `StorageError`, surfacing Postgres
/// unique-violations (SQLSTATE `23505`) as the typed
/// [`StorageError::UniqueViolation`] variant. The payload is the
/// constraint name (e.g. `users_email_key`) — NOT the full
/// Postgres message, which can echo the conflicting value.
///
/// Use this anywhere a write may collide with a `UNIQUE` /
/// `PRIMARY KEY` constraint so the auth crate can pattern-match
/// on the constraint without parsing SQLSTATE strings itself.
pub(crate) fn map_unique_violation(err: sqlx::Error) -> StorageError {
    if let sqlx::Error::Database(db_err) = &err
        && db_err.code().as_deref() == Some("23505")
    {
        return StorageError::UniqueViolation(db_err.constraint().unwrap_or("unknown").to_owned());
    }
    StorageError::from(err)
}
