-- #4.7 rate-limit fixed-window counters.
--
-- Fallback storage for the per-key fixed-window rate limiter
-- when DragonflyDB / Redis isn't available (personal-mode
-- deployments, dev laptops, small self-hosts). The auth crate's
-- `PgRateLimiter` uses `INSERT ... ON CONFLICT DO UPDATE` to
-- atomically read-and-bump the current window's counter in a
-- single statement; the UPSERT overwrites stale window starts
-- in-place for keys that keep getting traffic, but a key that
-- stops being used leaves its row behind. Without a periodic
-- sweep, table size therefore grows monotonically with the
-- count of distinct keys (IPs, sessions, …) ever observed —
-- not bounded by the active set.
--
-- GC: `RateLimitRepo::sweep_expired` deletes rows whose
-- `window_start` is older than a caller-chosen cutoff. The
-- scheduled job that calls it doesn't ship in this PR (no
-- cron / task-scheduler wiring yet); operators running this
-- backend should plan to invoke it on a periodic timer
-- (recommended cadence: hourly with `cutoff = now - 1 hour`).
-- A future ETL / housekeeping milestone wires the sweep
-- automatically.
--
-- The key is a free-form TEXT shaped by the caller as
-- `<kind>:<id>` (e.g. `ip:203.0.113.42`, `key:<uuid>`,
-- `tool:query_rows:<uuid>`) — the storage layer is opaque to
-- the kind so adding a new layer (e.g. per-org) is a one-line
-- caller change rather than a migration.
--
-- DRAGONFLY PARITY: a future Redis/Dragonfly-backed impl uses
-- `INCR rl:<key>:<window>` + `EXPIRE rl:<key>:<window> 60`,
-- which is semantically equivalent to this fixed-window UPSERT
-- modulo the eviction strategy. Switching backends doesn't
-- require schema changes — the `RateLimitRepo` trait is the
-- contract.

CREATE TABLE rate_limit_counters (
    -- `<kind>:<id>` (e.g. `ip:203.0.113.42`, `key:<uuid>`,
    -- `tool:query_rows:<uuid>`). Variable-length TEXT keeps
    -- the schema agnostic to whatever shape the caller picks;
    -- the WHERE clause keys on the literal string so kind
    -- changes don't need a migration.
    key             TEXT         PRIMARY KEY,
    -- Start of the active window. Updated to the new window's
    -- start whenever a request comes in after the prior window
    -- expired — the UPSERT in `check_and_increment` does this
    -- atomically with the counter reset, so a sustained-low
    -- traffic key doesn't accumulate stale rows.
    window_start    TIMESTAMPTZ  NOT NULL,
    -- Count of requests observed inside the active window. The
    -- UPSERT branches: same window → `count + 1`, new window →
    -- `1`. Negative values are unreachable but the CHECK below
    -- catches a future bug that bypasses the UPSERT.
    count           INTEGER      NOT NULL,

    CONSTRAINT rate_limit_counters_count_nonneg CHECK (count >= 0)
);

-- Sweep candidates for the eventual GC job — rows whose
-- window_start is more than one window-width in the past will
-- never be read again. Partial index isn't justified because
-- `count` doesn't help eviction; a plain btree on
-- `window_start` is enough for `DELETE … WHERE window_start
-- < $cutoff`.
CREATE INDEX rate_limit_counters_window_start_idx
    ON rate_limit_counters (window_start);
