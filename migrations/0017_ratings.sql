-- #5a.5 ratings — 5-star score on community-facing rows
-- (datasets / tools / connectors / playgrounds) with cached
-- aggregates.
--
-- Two tables:
--
--   1. `ratings` — one row per (user, target). UNIQUE drives
--      the idempotent upsert; the FK to `users` cascades so
--      account deletion takes the rating with it.
--   2. `rating_aggregates` — cached `avg_score` + `rating_count`
--      keyed on `(target_kind, target_id)`. Refreshed on every
--      write inside the same transaction (per-target advisory
--      lock + INSERT ... ON CONFLICT DO UPDATE — see
--      `refresh_aggregate` in storage::rating_repo) so dataset-
--      page renders can read a single row instead of running an
--      aggregation per page-load. A nightly cron-driven full
--      recompute lands in M5b along with the connector
--      framework — until then the on-write refresh is the only
--      source.

CREATE TABLE ratings (
    id              UUID         PRIMARY KEY DEFAULT uuidv7(),
    user_id         UUID         NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    target_kind     TEXT         NOT NULL,
    target_id       UUID         NOT NULL,
    -- 1-5 inclusive. SMALLINT is plenty and stays small in the
    -- bloat-prone `(user_id, target_kind, target_id, score)`
    -- access pattern.
    score           SMALLINT     NOT NULL,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT ratings_score_in_range CHECK (score BETWEEN 1 AND 5),
    CONSTRAINT ratings_target_kind_known CHECK (
        target_kind IN ('dataset', 'tool', 'connector', 'playground')
    ),
    -- One rating per user per target. Re-rating overwrites
    -- via ON CONFLICT.
    CONSTRAINT ratings_unique_per_user_target UNIQUE (user_id, target_kind, target_id)
);

-- Listing path — "what has this user rated?" (account page).
-- Composite ordered by `updated_at DESC` so the index covers
-- the filter + sort directly.
CREATE INDEX ratings_user_updated_idx
    ON ratings (user_id, updated_at DESC);

-- Aggregation path — supports the on-write refresh below
-- (`SELECT AVG, COUNT WHERE target_kind = $1 AND target_id = $2`).
CREATE INDEX ratings_target_idx
    ON ratings (target_kind, target_id);

COMMENT ON TABLE ratings IS
    'Per-user 5-star score on community-facing rows. UNIQUE per (user, target_kind, target_id).';

-- Cached aggregate per target. A row is upserted on every
-- write to `ratings`; a withdrawn rating may leave a row
-- behind with `rating_count = 0` (and `avg_score = 0`) — the
-- gateway treats that as "no ratings yet" identically to a
-- missing row, so we don't need an extra DELETE pass.
CREATE TABLE rating_aggregates (
    target_kind         TEXT             NOT NULL,
    target_id           UUID             NOT NULL,
    -- DOUBLE PRECISION (f64) over NUMERIC(3,2) because the
    -- workspace's sqlx build doesn't pull in bigdecimal /
    -- rust_decimal — the extra decimal-encoding round-trip
    -- isn't worth it for a 1-5 scale.
    avg_score           DOUBLE PRECISION NOT NULL DEFAULT 0,
    rating_count        INTEGER          NOT NULL DEFAULT 0,
    last_refreshed_at   TIMESTAMPTZ      NOT NULL DEFAULT now(),
    PRIMARY KEY (target_kind, target_id),
    CONSTRAINT rating_aggregates_target_kind_known CHECK (
        target_kind IN ('dataset', 'tool', 'connector', 'playground')
    ),
    CONSTRAINT rating_aggregates_count_non_negative CHECK (rating_count >= 0),
    CONSTRAINT rating_aggregates_avg_in_range CHECK (avg_score BETWEEN 0 AND 5)
);

COMMENT ON TABLE rating_aggregates IS
    'Cached avg+count per (target_kind, target_id). Refreshed on every ratings write; nightly recompute lives in M5b.';
