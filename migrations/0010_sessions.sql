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
    -- session is still valid (subject to expires_at AND
    -- absolute_expires_at). The lookup predicate is
    -- `revoked_at IS NULL AND expires_at > now() AND
    -- absolute_expires_at > now()` — revoke, idle expiry, and
    -- hard cap all surface the same way (no row returned).
    revoked_at        TIMESTAMPTZ,
    -- Best-effort audit metadata. NULL when the gateway couldn't
    -- determine the value (e.g. a misconfigured reverse proxy
    -- doesn't forward Client-IP). Neither column is load-bearing
    -- for authentication; rotating them later is safe.
    user_agent        TEXT,
    ip_addr           INET,

    CONSTRAINT sessions_id_sha256 CHECK (octet_length(id) = 32),
    -- Enforce the documented invariant "idle expiry never exceeds
    -- the hard cap": the auth crate composes `expires_at` via
    -- `LEAST(GREATEST($new, expires_at), absolute_expires_at)`,
    -- but a future writer that bypasses that helper (manual SQL,
    -- a migration backfill, a different language client) would
    -- otherwise be free to insert an `expires_at >
    -- absolute_expires_at` row. The CHECK pushes the invariant
    -- down to the storage engine so the lookup predicate
    -- `expires_at > $now AND absolute_expires_at > $now` can be
    -- read as "expires_at > $now implies absolute_expires_at >
    -- $now" without a database-wide audit.
    CONSTRAINT sessions_idle_within_absolute
        CHECK (expires_at <= absolute_expires_at)
);

-- "All sessions for user X" — used by logout-everywhere and by
-- the "active sessions" UI in #4.6.
CREATE INDEX sessions_user_id_idx ON sessions (user_id);

-- Sweep candidates for the GC job over EXPIRED-BUT-UNREVOKED
-- sessions. The partial predicate (`revoked_at IS NULL`)
-- intentionally excludes revoked rows: they are
-- comparatively low-volume (driven by explicit user action,
-- not by traffic) and the existing `sessions_user_id_idx`
-- already serves the "revoke all for user X" sweep without
-- needing a second partial index. If revoked-row GC becomes a
-- hotspot we can add `WHERE revoked_at IS NOT NULL` later; for
-- now keeping a single narrow index minimises write
-- amplification on every authenticated request.
CREATE INDEX sessions_expired_active_idx
    ON sessions (expires_at)
    WHERE revoked_at IS NULL;
