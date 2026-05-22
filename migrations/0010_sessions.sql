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
-- Two expiry columns capture the spec's "sliding window refresh
-- on each request (max 14d total)":
--
--   * `expires_at` — sliding-window idle expiry. Updated on each
--     authenticated request to `min(now + idle_ttl,
--     absolute_expires_at)`. An idle user is cleaned up after
--     idle_ttl elapses without activity.
--   * `absolute_expires_at` — hard cap on session lifetime.
--     Set at insert (`created_at + absolute_max`), never extended.
--     Even an actively-used session gets killed at this point.
--
-- `last_seen_at` is touched on each request for audit /
-- idle-session analytics; the validity predicate keys on
-- `expires_at` + `absolute_expires_at` + `revoked_at`.

CREATE TABLE sessions (
    -- 32-byte SHA-256 of the cleartext opaque token. The CHECK
    -- catches a future bug that persists a shorter or differently-
    -- encoded value (e.g. an accidental hex string instead of raw
    -- bytes).
    id                BYTEA        PRIMARY KEY,
    user_id           UUID         NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),
    last_seen_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    -- Sliding-window idle expiry. The auth crate touches this on
    -- each authenticated request to `min(now + idle_ttl,
    -- absolute_expires_at)` — never past the hard cap below.
    expires_at        TIMESTAMPTZ  NOT NULL,
    -- Hard cap on session lifetime. Set at insert
    -- (`created_at + absolute_max`); NEVER extended. Even an
    -- actively-used session dies once `now > absolute_expires_at`.
    -- Spec ("max 14d total") lives here.
    absolute_expires_at TIMESTAMPTZ NOT NULL,
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
