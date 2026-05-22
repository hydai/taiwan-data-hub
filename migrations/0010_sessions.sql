-- #4.5 session middleware.
--
-- Server-side sessions for the gateway's HTTP surface. The cookie
-- (`tdh_session`) carries an OPAQUE token; the DB primary key is
-- SHA-256 of that token. That means:
--
--   * A DB leak yields hashes, not working tokens.
--   * The cleartext token only ever lives on the client and in
--     transit (over TLS + httpOnly + Secure cookie attrs set by
--     the gateway).
--   * Logout / revocation is server-side and immediate (set
--     `revoked_at` and the next request fails the lookup),
--     unlike a stateless JWT where the only way to invalidate a
--     pre-expiry token is a blocklist.
--
-- Absolute lifetime is encoded in `expires_at` (set at insert to
-- `created_at + ttl`); each authenticated request updates
-- `last_seen_at` for audit / idle-session analytics. The DoD for
-- #4.5 says "max 14d total", so `expires_at` is NOT extended on
-- access — the session simply expires N days after first issue.

CREATE TABLE sessions (
    -- 32-byte SHA-256 of the cleartext opaque token. The CHECK
    -- catches a future bug that persists a shorter or differently-
    -- encoded value (e.g. an accidental hex string instead of raw
    -- bytes).
    id                BYTEA        PRIMARY KEY,
    user_id           UUID         NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),
    last_seen_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    -- Absolute expiry (created_at + ttl). Not extended on each
    -- access — matches the DoD's "max 14d total" requirement.
    expires_at        TIMESTAMPTZ  NOT NULL,
    -- Set on logout / forced revocation. A NULL value means the
    -- session is still valid (subject to expires_at). The lookup
    -- predicate filters on `revoked_at IS NULL AND expires_at >
    -- now()` so revoke + expiry both surface the same way to
    -- callers.
    revoked_at        TIMESTAMPTZ,
    -- Best-effort audit metadata. NULL when the gateway couldn't
    -- determine the value (e.g. a misconfigured reverse proxy
    -- doesn't forward Client-IP). Neither column is load-bearing
    -- for authentication; rotating them later is safe.
    user_agent        TEXT,
    ip_addr           INET,

    CONSTRAINT sessions_id_sha256 CHECK (octet_length(id) = 32)
);

-- "All sessions for user X" — used by logout-everywhere and by
-- the "active sessions" UI in #4.6.
CREATE INDEX sessions_user_id_idx ON sessions (user_id);

-- Sweep candidates for the eventual GC job (drop expired/revoked
-- rows). Partial index so the planner can do a direct range scan
-- without filtering rows whose lookup path already excludes them.
CREATE INDEX sessions_expired_active_idx
    ON sessions (expires_at)
    WHERE revoked_at IS NULL;
