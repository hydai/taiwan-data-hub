//! `users` + `auth_tokens` repositories (#4.2).
//!
//! Mirrors the trait + struct pattern used by `dataset_repo`:
//! traits for the operations, [`Storage`] for the sqlx-backed
//! implementation, plain data types for callers. The auth crate
//! consumes the traits so its business logic can be unit-tested
//! against an in-memory fake without touching Postgres.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::postgres::PgRow;
use uuid::Uuid;

use crate::{Storage, StorageError};

/// Discriminator stored in `auth_tokens.kind`.
///
/// Kept as a Rust enum + a single [`Self::as_str`] write-side
/// boundary so callers cannot insert a typo. The column never
/// round-trips back through Rust on the read path — `consume_auth_token`
/// returns only the owning `user_id` — so a `FromStr` decoder
/// would have no caller; if a future flow needs to read `kind`
/// back, add `FromStr` then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthTokenKind {
    /// Newly-registered user clicking the verify-your-email link.
    EmailVerify,
    /// User completing the "I forgot my password" flow.
    PasswordReset,
}

impl AuthTokenKind {
    /// Wire representation matching the
    /// `auth_tokens_kind_known` CHECK constraint in migration 0008.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EmailVerify => "email_verify",
            Self::PasswordReset => "password_reset",
        }
    }
}

/// A row of the `users` table.
#[derive(Debug, Clone)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub password_hash: String,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl User {
    fn from_row(row: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            email: row.try_get("email")?,
            password_hash: row.try_get("password_hash")?,
            email_verified_at: row.try_get("email_verified_at")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

/// Operations on the `users` table consumed by the auth crate.
#[async_trait]
pub trait UserRepo: Send + Sync {
    /// Insert a fresh user. Returns [`StorageError::UniqueViolation`]
    /// when the email collides — the auth service maps that to
    /// `AuthError::EmailTaken` and then to the uniform HTTP
    /// response.
    async fn insert_user(&self, email: &str, password_hash: &str) -> Result<User, StorageError>;

    /// Look up by exact email (case-insensitive — the column is
    /// `CITEXT`, the comparison is folded server-side).
    async fn find_user_by_email(&self, email: &str) -> Result<Option<User>, StorageError>;

    /// Look up by primary key. Used by the token-redemption path
    /// after `consume_auth_token` returns the owning `user_id`.
    async fn find_user_by_id(&self, id: Uuid) -> Result<Option<User>, StorageError>;

    /// Set `email_verified_at = now()`. Idempotent: a second call
    /// after a successful verify returns `Ok(false)` and leaves
    /// the timestamp unchanged.
    async fn mark_email_verified(&self, id: Uuid) -> Result<bool, StorageError>;

    /// Overwrite the password hash. Returns `Ok(true)` when the
    /// row exists.
    async fn update_password_hash(
        &self,
        id: Uuid,
        password_hash: &str,
    ) -> Result<bool, StorageError>;
}

/// Operations on `auth_tokens` consumed by the auth crate.
#[async_trait]
pub trait AuthTokenRepo: Send + Sync {
    /// Persist a freshly-generated single-use token.
    async fn insert_auth_token(
        &self,
        user_id: Uuid,
        kind: AuthTokenKind,
        token_hash: &[u8],
        expires_at: DateTime<Utc>,
    ) -> Result<(), StorageError>;

    /// Atomically consume a token matching `(kind, token_hash)`
    /// that hasn't expired and hasn't been consumed yet. Returns
    /// the owning `user_id` on success, `Ok(None)` when no row
    /// matched (caller maps to `AuthError::InvalidToken`).
    ///
    /// `now` is taken as a parameter so the consume cutoff is
    /// stable across the test fixture and the real wall clock.
    async fn consume_auth_token(
        &self,
        kind: AuthTokenKind,
        token_hash: &[u8],
        now: DateTime<Utc>,
    ) -> Result<Option<Uuid>, StorageError>;
}

#[async_trait]
impl UserRepo for Storage {
    async fn insert_user(&self, email: &str, password_hash: &str) -> Result<User, StorageError> {
        let row = sqlx::query(
            "INSERT INTO users (email, password_hash)
             VALUES ($1, $2)
             RETURNING id, email, password_hash, email_verified_at, created_at, updated_at",
        )
        .bind(email)
        .bind(password_hash)
        .fetch_one(self.pool())
        .await
        .map_err(map_insert_user_error)?;
        let user = User::from_row(&row).map_err(StorageError::from)?;
        Ok(user)
    }

    async fn find_user_by_email(&self, email: &str) -> Result<Option<User>, StorageError> {
        let row = sqlx::query(
            "SELECT id, email, password_hash, email_verified_at, created_at, updated_at
             FROM users WHERE email = $1",
        )
        .bind(email)
        .fetch_optional(self.pool())
        .await?;
        row.map(|r| User::from_row(&r).map_err(StorageError::from))
            .transpose()
    }

    async fn find_user_by_id(&self, id: Uuid) -> Result<Option<User>, StorageError> {
        let row = sqlx::query(
            "SELECT id, email, password_hash, email_verified_at, created_at, updated_at
             FROM users WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        row.map(|r| User::from_row(&r).map_err(StorageError::from))
            .transpose()
    }

    async fn mark_email_verified(&self, id: Uuid) -> Result<bool, StorageError> {
        let result = sqlx::query(
            "UPDATE users SET email_verified_at = now()
             WHERE id = $1 AND email_verified_at IS NULL",
        )
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn update_password_hash(
        &self,
        id: Uuid,
        password_hash: &str,
    ) -> Result<bool, StorageError> {
        let result = sqlx::query("UPDATE users SET password_hash = $1 WHERE id = $2")
            .bind(password_hash)
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(result.rows_affected() > 0)
    }
}

#[async_trait]
impl AuthTokenRepo for Storage {
    async fn insert_auth_token(
        &self,
        user_id: Uuid,
        kind: AuthTokenKind,
        token_hash: &[u8],
        expires_at: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO auth_tokens (user_id, kind, token_hash, expires_at)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(user_id)
        .bind(kind.as_str())
        .bind(token_hash)
        .bind(expires_at)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    async fn consume_auth_token(
        &self,
        kind: AuthTokenKind,
        token_hash: &[u8],
        now: DateTime<Utc>,
    ) -> Result<Option<Uuid>, StorageError> {
        // Race-guard predicate lives inline on the UPDATE so two
        // concurrent redemptions of the same link can't both win.
        // `RETURNING user_id` is `None` when the row didn't qualify.
        let row = sqlx::query(
            "UPDATE auth_tokens
                SET consumed_at = $4
              WHERE kind = $1
                AND token_hash = $2
                AND consumed_at IS NULL
                AND expires_at > $3
              RETURNING user_id",
        )
        .bind(kind.as_str())
        .bind(token_hash)
        .bind(now)
        .bind(now)
        .fetch_optional(self.pool())
        .await?;
        row.map(|r| r.try_get::<Uuid, _>("user_id").map_err(StorageError::from))
            .transpose()
    }
}

fn map_insert_user_error(err: sqlx::Error) -> StorageError {
    // sqlx surfaces the Postgres unique-violation as
    // `Error::Database(_)` with SQLSTATE `23505`. We map that to a
    // typed variant so the auth crate doesn't have to know about
    // SQLSTATE values.
    if let sqlx::Error::Database(db_err) = &err
        && db_err.code().as_deref() == Some("23505")
    {
        return StorageError::UniqueViolation(db_err.message().to_owned());
    }
    StorageError::from(err)
}
