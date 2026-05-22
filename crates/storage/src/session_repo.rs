//! `sessions` table repository (#4.5).
//!
//! Server-side session store. The cookie carries an opaque token;
//! the DB primary key is `sha256(token)` so a DB leak doesn't
//! yield working tokens. The auth crate builds the cipher /
//! cookie format on top; this module is the thin sqlx-backed
//! row store.

use std::net::IpAddr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::sqlx_errors::map_unique_violation;
use crate::{Storage, StorageError};

/// Input to [`SessionRepo::insert_session`].
#[derive(Debug, Clone)]
pub struct NewSession {
    /// SHA-256 of the cleartext opaque session token. The
    /// cleartext lives only in the cookie + RAM during the
    /// request that mints it.
    pub id_hash: Vec<u8>,
    pub user_id: Uuid,
    /// Sliding-window idle expiry. Set initially to `min(now +
    /// idle_ttl, absolute_expires_at)`; [`SessionRepo::
    /// touch_and_authenticate`] then advances this on each
    /// request, always capped at `absolute_expires_at`.
    pub expires_at: DateTime<Utc>,
    /// Hard cap on session lifetime. Set once at insert
    /// (`now + absolute_max`); NEVER extended. Even active
    /// sessions die at this point.
    pub absolute_expires_at: DateTime<Utc>,
    /// Best-effort audit fields. `None` when the gateway can't
    /// determine the value (no proxy header, missing UA).
    pub user_agent: Option<String>,
    pub ip_addr: Option<IpAddr>,
}

/// Row returned by [`SessionRepo::touch_and_authenticate`] on a
/// valid lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedSession {
    pub user_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[async_trait]
pub trait SessionRepo: Send + Sync {
    /// Persist a freshly-minted session. The caller pre-hashed
    /// the cleartext token; this row keys on the digest so the
    /// cleartext never lives in the DB.
    async fn insert_session(&self, new: NewSession) -> Result<(), StorageError>;

    /// Look up the session by `id_hash`, touch `last_seen_at`
    /// to `now`, **extend `expires_at` to `new_expires_at`**, and
    /// return the bound `(user_id, created_at, expires_at)` IF
    /// the row is unexpired AND not revoked.
    ///
    /// The extension is the sliding-window refresh: each
    /// authenticated request bumps the expiry forward by the
    /// service-defined TTL. The caller computes
    /// `new_expires_at` from its own clock so the storage layer
    /// stays clock-agnostic. Returns `Ok(None)` for: row
    /// missing, row revoked, row expired. The discrimination is
    /// deliberately collapsed at the trait surface — the caller
    /// treats all three the same (clear the cookie, return
    /// anonymous).
    async fn touch_and_authenticate(
        &self,
        id_hash: &[u8],
        now: DateTime<Utc>,
        new_expires_at: DateTime<Utc>,
    ) -> Result<Option<AuthenticatedSession>, StorageError>;

    /// Revoke a specific session. Returns `true` if a row was
    /// flipped (idempotent: already-revoked rows return false
    /// without error).
    async fn revoke_session(
        &self,
        id_hash: &[u8],
        now: DateTime<Utc>,
    ) -> Result<bool, StorageError>;

    /// Revoke every active session belonging to `user_id`. Used
    /// by "log out everywhere" and by the password-change flow
    /// to evict any stolen-token attackers. Returns the count of
    /// rows newly revoked.
    async fn revoke_all_sessions_for_user(
        &self,
        user_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<u64, StorageError>;
}

#[async_trait]
impl SessionRepo for Storage {
    async fn insert_session(&self, new: NewSession) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO sessions
                (id, user_id, expires_at, absolute_expires_at,
                 user_agent, ip_addr)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&new.id_hash)
        .bind(new.user_id)
        .bind(new.expires_at)
        .bind(new.absolute_expires_at)
        .bind(new.user_agent.as_deref())
        .bind(new.ip_addr)
        .execute(self.pool())
        .await
        // Maps SQLSTATE 23505 (a colliding `id` PK from a freak
        // RNG collision) to the typed `UniqueViolation` so the
        // caller can retry with a fresh token instead of
        // surfacing a Postgres detail string.
        .map_err(map_unique_violation)?;
        Ok(())
    }

    async fn touch_and_authenticate(
        &self,
        id_hash: &[u8],
        now: DateTime<Utc>,
        new_expires_at: DateTime<Utc>,
    ) -> Result<Option<AuthenticatedSession>, StorageError> {
        // `UPDATE … RETURNING` does the validity check + touch +
        // sliding-window slide + absolute cap + read in a single
        // statement. Three things have to be true for a hit:
        //
        //   * `revoked_at IS NULL`  — not logged out.
        //   * `expires_at > $now`   — idle window hasn't expired.
        //   * `absolute_expires_at > $now` — hard cap not exceeded.
        //
        // The SLIDE is `LEAST(GREATEST($3, expires_at),
        // absolute_expires_at)` — monotonic and capped. The
        // `GREATEST` defends against concurrent requests with
        // mildly-skewed clocks: a request whose `$3` is slightly
        // smaller than the row's current `expires_at` would
        // otherwise SHRINK the expiry and log the user out
        // early. With `GREATEST`, the slide can only advance.
        // The `LEAST(..., absolute_expires_at)` cap stops the
        // slide from ever exceeding the hard ceiling.
        // `absolute_expires_at` itself is never touched here.
        let row = sqlx::query_as::<_, (Uuid, DateTime<Utc>, DateTime<Utc>)>(
            "UPDATE sessions
                SET last_seen_at = $2,
                    expires_at   = LEAST(GREATEST($3, expires_at), absolute_expires_at)
              WHERE id = $1
                AND revoked_at IS NULL
                AND expires_at > $2
                AND absolute_expires_at > $2
              RETURNING user_id, created_at, expires_at",
        )
        .bind(id_hash)
        .bind(now)
        .bind(new_expires_at)
        .fetch_optional(self.pool())
        .await?;
        Ok(
            row.map(|(user_id, created_at, expires_at)| AuthenticatedSession {
                user_id,
                created_at,
                expires_at,
            }),
        )
    }

    async fn revoke_session(
        &self,
        id_hash: &[u8],
        now: DateTime<Utc>,
    ) -> Result<bool, StorageError> {
        // `revoked_at IS NULL` in the WHERE makes the operation
        // idempotent: re-revoking returns 0 rows without error.
        let result = sqlx::query(
            "UPDATE sessions
                SET revoked_at = $2
              WHERE id = $1
                AND revoked_at IS NULL",
        )
        .bind(id_hash)
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn revoke_all_sessions_for_user(
        &self,
        user_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<u64, StorageError> {
        // The doc says "active": unrevoked AND unexpired. Filter
        // on `expires_at > now` so we don't bump `revoked_at` on
        // rows that are already effectively dead — keeps the
        // returned count meaningful (newly-revoked, not "rows
        // touched including zombies").
        let result = sqlx::query(
            "UPDATE sessions
                SET revoked_at = $2
              WHERE user_id = $1
                AND revoked_at IS NULL
                AND expires_at > $2",
        )
        .bind(user_id)
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected())
    }
}
