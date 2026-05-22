//! `mcp_api_keys` repository (#4.6).
//!
//! Per-user API keys for programmatic access. The cleartext key
//! is shown ONCE on creation; the DB only holds the SHA-256 hash
//! plus a short public prefix. The auth crate builds the
//! key-format / verification on top; this module is the thin
//! sqlx-backed row store.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::sqlx_errors::map_unique_violation;
use crate::{Storage, StorageError};

/// Input to [`ApiKeyRepo::insert_api_key`].
#[derive(Debug, Clone)]
pub struct NewApiKey {
    pub user_id: Uuid,
    /// User-supplied label ("laptop", "ci-runner"). Free text; the
    /// storage layer doesn't interpret it.
    pub name: String,
    /// First N bytes of the cleartext key, kept in plaintext so
    /// the UI can disambiguate keys without re-showing the secret.
    pub key_prefix: String,
    /// 32-byte SHA-256 of the cleartext key. The lookup path
    /// reads this column directly; the cleartext itself only
    /// lives in the one-time creation response.
    pub key_hash: Vec<u8>,
    /// Scope set this key carries. Empty array = no elevated
    /// capabilities.
    pub scopes: Vec<String>,
    /// Rate-limit tier (`free` / `pro` / `enterprise`). The
    /// table-level CHECK constraint enforces the allowed set.
    pub rate_limit_tier: String,
    /// Wall-clock `now` the auth crate captured for this issue.
    /// Bound to `created_at` so it shares the clock source with
    /// every subsequent `last_used_at` / `revoked_at` update
    /// (which the auth crate also passes `Utc::now()` into).
    /// Mirrors the #4.5 session-row pattern: under app/DB clock
    /// skew the audit timeline `created_at <= last_used_at`
    /// would otherwise be violatable on the very first touch.
    /// The table's `DEFAULT now()` stays as a safety net for
    /// direct SQL writers that don't supply a value.
    pub created_at: DateTime<Utc>,
}

/// Row shape returned by list / verify lookups. Cleartext key is
/// NEVER part of this — it only exists in the one-time creation
/// response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub key_prefix: String,
    pub scopes: Vec<String>,
    pub rate_limit_tier: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[async_trait]
pub trait ApiKeyRepo: Send + Sync {
    /// Persist a freshly-minted API key. The caller pre-hashed
    /// the cleartext; this row keys on the digest so the
    /// cleartext never lives in the DB.
    async fn insert_api_key(&self, new: NewApiKey) -> Result<Uuid, StorageError>;

    /// Look up a key by `key_hash`, touch `last_used_at` to `now`,
    /// and return the row IF the key is unrevoked. The touch +
    /// validity check happen in a single `UPDATE … RETURNING` so
    /// a concurrent revoke between SELECT and UPDATE can't yield
    /// a stale "still valid" result. `last_used_at` is clamped
    /// via `GREATEST` to stay monotonic under multi-instance
    /// clock skew (mirrors the `sessions` table pattern).
    async fn touch_and_verify(
        &self,
        key_hash: &[u8],
        now: DateTime<Utc>,
    ) -> Result<Option<ApiKeyRow>, StorageError>;

    /// List every key owned by `user_id`, ordered by `created_at
    /// DESC` so the most-recently-created key appears first
    /// (matches the Account UI's expected display order).
    /// Revoked keys are INCLUDED — the UI distinguishes them via
    /// `revoked_at IS NOT NULL` so users can see their
    /// revocation history without a separate "deleted" toggle.
    async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<ApiKeyRow>, StorageError>;

    /// Revoke a specific key. Returns the revoked row if a row
    /// was flipped (idempotent: already-revoked rows return
    /// `Ok(None)` without error, mirroring the session repo's
    /// behaviour). The `user_id` parameter scopes the revoke
    /// to the owner — passing someone else's `id` returns
    /// `Ok(None)` so the gateway endpoint can fold "not yours"
    /// and "not found" into the same response.
    async fn revoke(
        &self,
        id: Uuid,
        user_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<Option<ApiKeyRow>, StorageError>;
}

#[async_trait]
impl ApiKeyRepo for Storage {
    async fn insert_api_key(&self, new: NewApiKey) -> Result<Uuid, StorageError> {
        // `id` defaults to `uuidv7()` from the table definition;
        // the RETURNING clause hands the generated UUID back to
        // the caller in a single round trip.
        //
        // `created_at` is bound EXPLICITLY from
        // `new.created_at` (the auth crate's single per-issue
        // `now`) so it shares the wall-clock source with every
        // subsequent `last_used_at` / `revoked_at` write —
        // mirroring the #4.5 session-row fix. The table's
        // `DEFAULT now()` remains as a safety net for direct
        // SQL writers (manual psql, future backfills) that
        // skip this column.
        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO mcp_api_keys
                (user_id, name, key_prefix, key_hash,
                 scopes, rate_limit_tier, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             RETURNING id",
        )
        .bind(new.user_id)
        .bind(&new.name)
        .bind(&new.key_prefix)
        .bind(&new.key_hash)
        .bind(&new.scopes)
        .bind(&new.rate_limit_tier)
        .bind(new.created_at)
        .fetch_one(self.pool())
        .await
        // SQLSTATE 23505 on the hash column means a SHA-256
        // collision (cryptographically astronomical) OR a bug
        // that minted the same key twice. Either way, the auth
        // crate retries with fresh entropy — matching the
        // session repo pattern.
        .map_err(map_unique_violation)?;
        Ok(row.0)
    }

    async fn touch_and_verify(
        &self,
        key_hash: &[u8],
        now: DateTime<Utc>,
    ) -> Result<Option<ApiKeyRow>, StorageError> {
        // Same single-statement pattern as the session repo:
        // validity check + touch + read in one UPDATE so a
        // concurrent revoke can't sneak a "valid" return past
        // the predicate. `last_used_at` is wrapped in
        // `GREATEST(last_used_at, $2)` so a request from a
        // gateway instance with a slightly-trailing clock
        // can't roll the audit timestamp backwards.
        // `COALESCE` handles the NULL case for the very first
        // request on a freshly-minted key.
        let row = sqlx::query_as::<_, ApiKeyRowSql>(
            "UPDATE mcp_api_keys
                SET last_used_at = GREATEST(COALESCE(last_used_at, $2), $2)
              WHERE key_hash = $1
                AND revoked_at IS NULL
              RETURNING id, user_id, name, key_prefix, scopes,
                        rate_limit_tier, created_at, last_used_at,
                        revoked_at",
        )
        .bind(key_hash)
        .bind(now)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(ApiKeyRow::from))
    }

    async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<ApiKeyRow>, StorageError> {
        let rows = sqlx::query_as::<_, ApiKeyRowSql>(
            "SELECT id, user_id, name, key_prefix, scopes,
                    rate_limit_tier, created_at, last_used_at,
                    revoked_at
               FROM mcp_api_keys
              WHERE user_id = $1
              ORDER BY created_at DESC",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.into_iter().map(ApiKeyRow::from).collect())
    }

    async fn revoke(
        &self,
        id: Uuid,
        user_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<Option<ApiKeyRow>, StorageError> {
        // `revoked_at IS NULL` in the WHERE makes the operation
        // idempotent — a second revoke on the same row returns
        // `None` rather than overwriting the original
        // revocation timestamp.
        let row = sqlx::query_as::<_, ApiKeyRowSql>(
            "UPDATE mcp_api_keys
                SET revoked_at = $3
              WHERE id = $1
                AND user_id = $2
                AND revoked_at IS NULL
              RETURNING id, user_id, name, key_prefix, scopes,
                        rate_limit_tier, created_at, last_used_at,
                        revoked_at",
        )
        .bind(id)
        .bind(user_id)
        .bind(now)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(ApiKeyRow::from))
    }
}

/// sqlx-decodable row mirror of [`ApiKeyRow`]. The `Vec<String>`
/// for scopes binds to `PostgreSQL` `TEXT[]` directly via the
/// `sqlx::FromRow` derive.
#[derive(Debug, sqlx::FromRow)]
struct ApiKeyRowSql {
    id: Uuid,
    user_id: Uuid,
    name: String,
    key_prefix: String,
    scopes: Vec<String>,
    rate_limit_tier: String,
    created_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
}

impl From<ApiKeyRowSql> for ApiKeyRow {
    fn from(value: ApiKeyRowSql) -> Self {
        Self {
            id: value.id,
            user_id: value.user_id,
            name: value.name,
            key_prefix: value.key_prefix,
            scopes: value.scopes,
            rate_limit_tier: value.rate_limit_tier,
            created_at: value.created_at,
            last_used_at: value.last_used_at,
            revoked_at: value.revoked_at,
        }
    }
}
