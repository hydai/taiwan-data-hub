-- #4.6 API key management.
--
-- Per-user API keys for programmatic access to the gateway (MCP
-- + REST). The cleartext key (`tdh_<base64url>`) is shown ONCE
-- on creation and only the SHA-256 hash + a short public
-- `key_prefix` live in the DB. That means:
--
--   * A DB leak yields hashes, not working keys.
--   * The account UI lists keys by `key_prefix` ("tdh_abcd…") so
--     a user can identify which row is which without ever
--     re-displaying the cleartext.
--   * Revocation flips `revoked_at`; the lookup predicate keys
--     on `key_hash` AND `revoked_at IS NULL`, so the next
--     authenticated request fails the lookup immediately
--     (unlike a stateless JWT key where rotation would need a
--     blocklist).
--
-- `last_used_at` is touched on every authenticated request that
-- carries the key (the #4.7 rate-limit middleware sets it). It
-- powers the "active sessions / unused keys" UI and lets users
-- spot stale keys to rotate.

CREATE TABLE mcp_api_keys (
    id              UUID         PRIMARY KEY DEFAULT uuidv7(),
    user_id         UUID         NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- User-supplied label ("laptop", "ci-runner", "vault"). Free
    -- text; the storage layer doesn't interpret it.
    name            TEXT         NOT NULL,
    -- First N bytes of the cleartext key, kept in plaintext so
    -- the UI can disambiguate rows ("tdh_a1b2…") without
    -- showing the secret. Stored as TEXT (not BYTEA) because
    -- it's a readable identifier, not raw entropy.
    key_prefix      TEXT         NOT NULL,
    -- 32-byte SHA-256 of the cleartext key. The lookup path
    -- `SELECT ... WHERE key_hash = $1 AND revoked_at IS NULL`
    -- only ever sees the hash; the cleartext lives in the
    -- client (and the one-time response that minted it).
    key_hash        BYTEA        NOT NULL,
    -- Scope set this key carries. Empty array means "no
    -- elevated capabilities, public-tool access only". The
    -- string values are interpreted at the auth layer (#4.7+);
    -- the storage layer is opaque so we don't need a migration
    -- every time a scope is added.
    scopes          TEXT[]       NOT NULL DEFAULT '{}',
    -- Rate-limit tier (`free` / `pro` / `enterprise`). The
    -- middleware reads this column on every authenticated
    -- request; the value set is intentionally a TEXT (not
    -- ENUM) so adding a new tier is a one-line CHECK update
    -- instead of an ALTER TYPE round trip.
    rate_limit_tier TEXT         NOT NULL DEFAULT 'free',
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    -- Touched by the #4.7 rate-limit middleware on every
    -- authenticated request that carries this key. NULL for a
    -- freshly-minted key that has not yet been used. The audit
    -- timeline `created_at <= last_used_at` is monotonic — the
    -- middleware uses `GREATEST(last_used_at, $now)` to defend
    -- against multi-instance clock skew, mirroring the
    -- `sessions` table pattern from #4.5.
    last_used_at    TIMESTAMPTZ,
    -- Set on revoke / rotate. A NULL value means the key is
    -- valid. The lookup predicate is `revoked_at IS NULL` so
    -- revoke is immediate (the next request fails the lookup).
    revoked_at      TIMESTAMPTZ,

    -- SHA-256 is always 32 bytes; CHECK catches a future bug
    -- that persists a shorter/longer value (e.g. an accidental
    -- hex string instead of raw bytes).
    CONSTRAINT mcp_api_keys_key_hash_sha256 CHECK (octet_length(key_hash) = 32),
    -- Tier set is closed at the moment; widening it is a one-
    -- line ALTER on the CONSTRAINT. ENUM would force an
    -- ALTER TYPE round trip and rewrite every catalog table
    -- that references it.
    CONSTRAINT mcp_api_keys_tier_allowed CHECK (
        rate_limit_tier IN ('free', 'pro', 'enterprise')
    )
);

-- "All keys for user X" — used by the Account page list view
-- and by the "revoke all for user" path in the password-reset
-- flow.
CREATE INDEX mcp_api_keys_user_id_idx ON mcp_api_keys (user_id);

-- Lookup-by-hash for the authenticated-request hot path. Partial
-- on `revoked_at IS NULL` because revoked keys never need to be
-- found by hash again — the only access path for a revoked row
-- is the Account UI ("you revoked this key on …"), which goes
-- through `mcp_api_keys_user_id_idx`.
CREATE INDEX mcp_api_keys_key_hash_active_idx
    ON mcp_api_keys (key_hash)
    WHERE revoked_at IS NULL;
