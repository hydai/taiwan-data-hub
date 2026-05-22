//! `oauth_states` + `oauth_accounts` repositories (#4.3, #4.4).
//!
//! Same trait + struct pattern as `auth_repo.rs`: traits for the
//! ops, `Storage` for the sqlx-backed impl, plain data types
//! for callers. The auth crate consumes the traits so unit tests
//! can use in-memory fakes.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

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
    /// `now` is taken as a parameter so the expiry cutoff is
    /// stable across test + production wall clocks.
    async fn consume_oauth_state(
        &self,
        state_hash: &[u8],
        now: DateTime<Utc>,
    ) -> Result<Option<OAuthPendingState>, StorageError>;
}

#[async_trait]
pub trait OAuthAccountRepo: Send + Sync {
    /// Upsert by (`provider`, `provider_user_id`). On conflict,
    /// rotates the encrypted tokens + bumps `updated_at`.
    async fn upsert_oauth_account(&self, new: NewOAuthAccount) -> Result<(), StorageError>;
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
        .await?;
        Ok(())
    }

    async fn consume_oauth_state(
        &self,
        state_hash: &[u8],
        now: DateTime<Utc>,
    ) -> Result<Option<OAuthPendingState>, StorageError> {
        // `DELETE … RETURNING` does the consume in a single
        // statement so a race between two concurrent callbacks
        // for the same state can't both win.
        let row = sqlx::query_as::<_, (Vec<u8>, String, String, String, DateTime<Utc>)>(
            "DELETE FROM oauth_states
              WHERE state_hash = $1 AND expires_at > $2
              RETURNING state_hash, code_verifier, provider, redirect_uri, expires_at",
        )
        .bind(state_hash)
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
        sqlx::query(
            "INSERT INTO oauth_accounts
                (user_id, provider, provider_user_id,
                 access_token_ciphertext, access_token_nonce,
                 refresh_token_ciphertext, refresh_token_nonce,
                 expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (provider, provider_user_id)
             DO UPDATE SET
                user_id = EXCLUDED.user_id,
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
        .await?;
        Ok(())
    }
}
