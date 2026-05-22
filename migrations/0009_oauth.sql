-- #4.3 GitHub OAuth (and #4.4 Google) — schema.
--
-- Two tables:
--
--   * `oauth_states` is a short-lived ledger that holds the
--     PKCE `code_verifier` (and a SHA-256 of the CSRF `state`
--     token) between the authorize-redirect and the callback.
--     Rows are deleted on consume; expired rows are cleaned up
--     by the etl-worker (v0.2 follow-up, same job that prunes
--     auth_tokens). PRIMARY KEY on `state_hash` so a callback
--     lookup is a single-row index probe.
--
--   * `oauth_accounts` is the per-(user, provider) link with
--     the AES-GCM-encrypted access token. The KEK is supplied
--     via env; each row carries its own 12-byte GCM nonce.
--     Refresh-token columns are nullable because GitHub OAuth
--     Apps don't issue refresh tokens.

CREATE TABLE oauth_states (
    -- SHA-256 of the cleartext state token — the cleartext only
    -- ever lives in the redirect URL we issue to the user.
    state_hash      BYTEA       PRIMARY KEY,
    -- PKCE code_verifier (kept cleartext; the value is single-
    -- use and dies with the row).
    code_verifier   TEXT        NOT NULL,
    provider        TEXT        NOT NULL,
    -- The redirect_uri we asked the provider to call back. Saved
    -- so the callback handler can echo the same value on the
    -- token-exchange POST (required by OAuth 2.1).
    redirect_uri    TEXT        NOT NULL,
    expires_at      TIMESTAMPTZ NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT oauth_states_provider_known CHECK (provider IN ('github', 'google')),
    CONSTRAINT oauth_states_state_hash_sha256 CHECK (octet_length(state_hash) = 32),
    -- RFC 7636 mandates 43–128 ASCII chars for `code_verifier`.
    -- The app always emits 43 (32-byte OsRng → base64url-no-pad);
    -- the CHECK catches a future bug that persists a shorter or
    -- absurdly long value before the row can ever be redeemed.
    CONSTRAINT oauth_states_code_verifier_len
        CHECK (char_length(code_verifier) BETWEEN 43 AND 128)
);

CREATE INDEX oauth_states_expires_idx
    ON oauth_states (expires_at);

CREATE TABLE oauth_accounts (
    user_id                 UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider                TEXT        NOT NULL,
    -- Provider-side stable id (GitHub: `user.id`, Google: `sub`).
    -- Distinct from the email because users can change emails.
    provider_user_id        TEXT        NOT NULL,
    -- AES-256-GCM ciphertext + 12-byte nonce. The plaintext is
    -- the provider's access token; the KEK lives in env. We
    -- store the nonce alongside so each row uses its own and
    -- the same KEK can decrypt every row.
    access_token_ciphertext BYTEA       NOT NULL,
    access_token_nonce      BYTEA       NOT NULL,
    -- Refresh-token columns are nullable. GitHub OAuth Apps
    -- don't issue refresh tokens; Google does, so #4.4 populates
    -- these. Same shape: ciphertext + 12-byte GCM nonce.
    refresh_token_ciphertext BYTEA,
    refresh_token_nonce      BYTEA,
    expires_at              TIMESTAMPTZ,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),

    PRIMARY KEY (provider, provider_user_id),
    -- One account per (user, provider) — a user has at most one
    -- GitHub identity, one Google identity, etc. Catches the
    -- "user re-authed with a different GitHub account that has
    -- the same email" race at INSERT time instead of letting two
    -- rows coexist.
    UNIQUE (user_id, provider),
    CONSTRAINT oauth_accounts_provider_known
        CHECK (provider IN ('github', 'google')),
    CONSTRAINT oauth_accounts_access_nonce_len
        CHECK (octet_length(access_token_nonce) = 12),
    CONSTRAINT oauth_accounts_refresh_nonce_len_when_set
        CHECK (refresh_token_nonce IS NULL OR octet_length(refresh_token_nonce) = 12),
    -- Refresh-token columns travel together — either both set or both NULL.
    CONSTRAINT oauth_accounts_refresh_pair
        CHECK ((refresh_token_ciphertext IS NULL) = (refresh_token_nonce IS NULL))
);

CREATE INDEX oauth_accounts_user_idx
    ON oauth_accounts (user_id);

-- Reuse the users `updated_at` trigger pattern from migration
-- 0008 so any UPDATE on oauth_accounts touches the column.
CREATE TRIGGER oauth_accounts_set_updated_at_trg
    BEFORE UPDATE ON oauth_accounts
    FOR EACH ROW
    EXECUTE FUNCTION users_set_updated_at();
