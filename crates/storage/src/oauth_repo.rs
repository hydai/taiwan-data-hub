//! `oauth_states` + `oauth_accounts` repositories (#4.3, #4.4).
//!
//! Same trait + struct pattern as `auth_repo.rs`: traits for the
//! ops, `Storage` for the sqlx-backed impl, plain data types
//! for callers. The auth crate consumes the traits so unit tests
//! can use in-memory fakes.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::sqlx_errors::map_unique_violation;
use crate::{Storage, StorageError};

/// One row of `oauth_states` — the short-lived ledger that
/// holds the PKCE code-verifier + CSRF state hash between the
/// authorize-redirect and the callback.
#[derive(Debug, Clone)]
pub struct OAuthPendingState {
    /// SHA-256 of the cleartext CSRF state.
    pub state_hash: Vec<u8>,
    /// PKCE `code_verifier` — sent to the provider on the
    /// token-exchange POST.
    pub code_verifier: String,
    /// Wire identifier — matches the
    /// `oauth_states_provider_known` CHECK.
    pub provider: String,
    /// Redirect URI the callback runs at. Must be echoed on the
    /// token-exchange POST (OAuth 2.1 requirement).
    pub redirect_uri: String,
    pub expires_at: DateTime<Utc>,
}

/// Input for [`OAuthAccountRepo::upsert_oauth_account`].
#[derive(Debug, Clone)]
pub struct NewOAuthAccount {
    pub user_id: Uuid,
    pub provider: String,
    pub provider_user_id: String,
    pub access_token_ciphertext: Vec<u8>,
    pub access_token_nonce: Vec<u8>,
    pub refresh_token_ciphertext: Option<Vec<u8>>,
    pub refresh_token_nonce: Option<Vec<u8>>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[async_trait]
pub trait OAuthStateRepo: Send + Sync {
    /// Persist a freshly-minted pending state. Caller pre-hashed
    /// the cleartext token; this row keys on the digest so the
    /// cleartext never lives in the DB.
    async fn insert_oauth_state(&self, pending: OAuthPendingState) -> Result<(), StorageError>;

    /// Atomically delete + return the matching pending row. Used
    /// by the callback to retrieve the PKCE code-verifier; the
    /// row is removed in the same statement so a replay returns
    /// `Ok(None)`.
    ///
    /// `provider` + `redirect_uri` are part of the predicate so
    /// a callback that arrived on the wrong service or with the
    /// wrong callback URL cannot consume someone else's pending
    /// state — an attacker who saw a state token can't force
    /// the legitimate user to restart.
    ///
    /// `now` is taken as a parameter so the expiry cutoff is
    /// stable across test + production wall clocks.
    async fn consume_oauth_state(
        &self,
        state_hash: &[u8],
        provider: &str,
        redirect_uri: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<OAuthPendingState>, StorageError>;
}

#[async_trait]
pub trait OAuthAccountRepo: Send + Sync {
    /// Upsert by (`provider`, `provider_user_id`). On conflict,
    /// rotates the encrypted tokens + bumps `updated_at` but
    /// leaves `user_id` untouched — the provider identity binds
    /// to the original user even if the auth service's
    /// email-based fallback would otherwise pick a different
    /// row. That defends against an account-takeover where an
    /// attacker can change their provider-side email to match
    /// an existing victim's address.
    async fn upsert_oauth_account(&self, new: NewOAuthAccount) -> Result<(), StorageError>;

    /// Look up the `user_id` for an existing
    /// (`provider`, `provider_user_id`) link. Returned by the
    /// auth service's `finish_login` BEFORE the email-based
    /// fallback so the provider identity stays bound to the
    /// original user.
    async fn find_user_id_by_provider_identity(
        &self,
        provider: &str,
        provider_user_id: &str,
    ) -> Result<Option<Uuid>, StorageError>;
}

#[async_trait]
impl OAuthStateRepo for Storage {
    async fn insert_oauth_state(&self, pending: OAuthPendingState) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO oauth_states
                (state_hash, code_verifier, provider, redirect_uri, expires_at)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&pending.state_hash)
        .bind(&pending.code_verifier)
        .bind(&pending.provider)
        .bind(&pending.redirect_uri)
        .bind(pending.expires_at)
        .execute(self.pool())
        .await
        // Maps SQLSTATE 23505 (e.g. a colliding `state_hash` PK)
        // to the typed `UniqueViolation` so callers can match on
        // the constraint without parsing Postgres detail strings.
        .map_err(map_unique_violation)?;
        Ok(())
    }

    async fn consume_oauth_state(
        &self,
        state_hash: &[u8],
        provider: &str,
        redirect_uri: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<OAuthPendingState>, StorageError> {
        // `DELETE … RETURNING` does the consume in a single
        // statement so a race between two concurrent callbacks
        // for the same state can't both win. `provider` and
        // `redirect_uri` are part of the predicate (not just
        // checked by the caller) so a callback on the wrong
        // service or with the wrong callback URL leaves the
        // row in place for the legitimate caller.
        let row = sqlx::query_as::<_, (Vec<u8>, String, String, String, DateTime<Utc>)>(
            "DELETE FROM oauth_states
              WHERE state_hash = $1
                AND provider = $2
                AND redirect_uri = $3
                AND expires_at > $4
              RETURNING state_hash, code_verifier, provider, redirect_uri, expires_at",
        )
        .bind(state_hash)
        .bind(provider)
        .bind(redirect_uri)
        .bind(now)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(
            |(state_hash, code_verifier, provider, redirect_uri, expires_at)| OAuthPendingState {
                state_hash,
                code_verifier,
                provider,
                redirect_uri,
                expires_at,
            },
        ))
    }
}

#[async_trait]
impl OAuthAccountRepo for Storage {
    async fn upsert_oauth_account(&self, new: NewOAuthAccount) -> Result<(), StorageError> {
        // `user_id` is deliberately ABSENT from the UPDATE set:
        // the binding made on the original INSERT stays put even
        // if the auth-service caller passes a different user_id
        // (which the service shouldn't, but the DB enforces it
        // independently as defense-in-depth).
        sqlx::query(
            "INSERT INTO oauth_accounts
                (user_id, provider, provider_user_id,
                 access_token_ciphertext, access_token_nonce,
                 refresh_token_ciphertext, refresh_token_nonce,
                 expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (provider, provider_user_id)
             DO UPDATE SET
                access_token_ciphertext = EXCLUDED.access_token_ciphertext,
                access_token_nonce = EXCLUDED.access_token_nonce,
                refresh_token_ciphertext = EXCLUDED.refresh_token_ciphertext,
                refresh_token_nonce = EXCLUDED.refresh_token_nonce,
                expires_at = EXCLUDED.expires_at",
        )
        .bind(new.user_id)
        .bind(&new.provider)
        .bind(&new.provider_user_id)
        .bind(&new.access_token_ciphertext)
        .bind(&new.access_token_nonce)
        .bind(new.refresh_token_ciphertext.as_deref())
        .bind(new.refresh_token_nonce.as_deref())
        .bind(new.expires_at)
        .execute(self.pool())
        .await
        // The primary (`provider`, `provider_user_id`) collision
        // is absorbed by `ON CONFLICT … DO UPDATE` above, so the
        // SQLSTATE 23505 we care about here is the
        // `oauth_accounts_user_id_provider_key` UNIQUE — i.e. the
        // attempt to link a SECOND provider identity to the same
        // user. Map it so the auth crate can surface a typed
        // "already-linked" error.
        .map_err(map_unique_violation)?;
        Ok(())
    }

    async fn find_user_id_by_provider_identity(
        &self,
        provider: &str,
        provider_user_id: &str,
    ) -> Result<Option<Uuid>, StorageError> {
        let row = sqlx::query_as::<_, (Uuid,)>(
            "SELECT user_id FROM oauth_accounts
              WHERE provider = $1 AND provider_user_id = $2",
        )
        .bind(provider)
        .bind(provider_user_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|(uid,)| uid))
    }
}
