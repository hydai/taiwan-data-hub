-- #5b.1 etl_dlq — dead-letter table for ETL crawl runs that
-- exhausted their retry budget.
--
-- One row per terminal failure. The worker wraps a unit
-- of work (today: a whole crawl pass — drain pagination,
-- resolve domains, upsert datasets — tagged
-- `job_kind = 'crawl_pass'`) in the retry-with-backoff
-- envelope (etl-worker/src/retry.rs); when the envelope
-- gives up after the configured attempts it writes one
-- row here. Transient transport errors that resolved on
-- a later attempt never land in the DLQ. Operators read
-- the DLQ to find sources that need manual attention;
-- rows stay until the operator explicitly sets
-- `resolved_at`, so the DLQ doubles as an audit trail.
--
-- `error_kind` is denormalised from
-- `connectors::ConnectorError` so SQL queries can
-- `WHERE error_kind = 'bad_status'` without parsing the
-- message. The first six categories below mirror
-- `ConnectorError` variants one-for-one; `other` is a
-- writer-side bucket for cases the worker's classifier
-- doesn't otherwise cover. The Rust-side
-- `DlqErrorKind::from_wire` decoder is STRICT: any value
-- it doesn't recognise is treated as CHECK drift and
-- raises `StorageError::Decode` (loud rather than silent).
-- Extending the enum requires lockstep updates to this
-- CHECK constraint, `DlqErrorKind`, and `from_wire`.
--
-- `payload` is a small JSONB blob (cursor, attempt
-- counter, http status, response body excerpt — connector
-- chooses). Keeping the schema loose here is intentional:
-- each connector emits the context that's useful for THAT
-- failure mode, and the DLQ readers (operators or future
-- alerting) treat it as a black box per row.

CREATE TABLE etl_dlq (
    id            UUID         PRIMARY KEY DEFAULT uuidv7(),
    source        TEXT         NOT NULL,
    -- A coarse operation tag. Today the worker writes
    -- `crawl_pass` for the whole `run_one_pass` (drain
    -- pagination → resolve domains → upsert). Future
    -- per-dataset envelopes will pick their own tags
    -- (e.g. `fetch_metadata`, `fetch_data`). Lets the
    -- operator query "all DLQ rows from the catalog
    -- walk vs. the per-dataset fetch" without parsing
    -- the payload. Free-form by design — adding a new
    -- operation kind shouldn't require a migration.
    job_kind      TEXT         NOT NULL,
    -- Total attempts the envelope made before giving up,
    -- including the failing one. Always ≥ 1.
    attempts      INTEGER      NOT NULL CHECK (attempts >= 1),
    -- Normalised error category. The first six values
    -- in the CHECK set below mirror
    -- `connectors::ConnectorError` variant names so the
    -- envelope can map without lossy stringification;
    -- `other` is a WRITER-side bucket the worker's
    -- classifier writes for cases it can't otherwise
    -- categorise. The Rust-side `DlqErrorKind::from_wire`
    -- READER is strict — any unrecognised value is
    -- treated as CHECK drift and raises
    -- `StorageError::Decode`. Extending the enum requires
    -- lockstep updates to this CHECK + `DlqErrorKind` +
    -- `from_wire` (see the table header for the full
    -- contract).
    error_kind    TEXT         NOT NULL,
    error_message TEXT         NOT NULL,
    payload       JSONB,
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT now(),
    -- Set when an operator (or a follow-up automation)
    -- has dispositioned the row. NULL = still actionable.
    resolved_at   TIMESTAMPTZ,
    -- Free-form note an operator can attach when they
    -- resolve. NULL = no note.
    resolution_note TEXT,
    CONSTRAINT etl_dlq_source_known CHECK (
        source IN ('data_gov_tw', 'twse', 'moea', 'cwa', 'fishery_moa', 'user_contrib')
    ),
    CONSTRAINT etl_dlq_error_kind_known CHECK (
        error_kind IN (
            'transport',
            'bad_status',
            'decode',
            'config',
            'invalid_cursor',
            'unsupported',
            'other'
        )
    ),
    -- A resolution note without a `resolved_at` is a
    -- partially-written row — reject at insert time.
    CONSTRAINT etl_dlq_resolution_atoms CHECK (
        resolution_note IS NULL OR resolved_at IS NOT NULL
    )
);

-- Open queue scan: "show me actionable DLQ rows newest
-- first". The list query paginates by UUIDv7 id (`WHERE
-- id < $cursor ORDER BY id DESC`) — UUIDv7 is time-
-- ordered, so id-DESC IS newest-first AND gives a strict
-- total order (no two rows with the same id can straddle
-- a page boundary). The index is on `id`, not `created_at`,
-- because PG's planner picks an index by column-match
-- against the WHERE / ORDER BY columns — a `created_at`
-- index can't serve an `id`-cursor query even though
-- UUIDv7 makes the two semantically equivalent. A B-tree
-- on `id` is scannable in either direction, so a plain
-- (id) index covers both ASC and DESC walks.
CREATE INDEX etl_dlq_open_idx
    ON etl_dlq (id)
    WHERE resolved_at IS NULL;

-- Per-source health view — "how many open DLQ rows does
-- this source have?". Drives the operator dashboard's
-- "sources needing attention" panel. Same `(source, id)`
-- shape as `etl_dlq_open_idx` so per-source pagination by
-- id-cursor stays index-only.
CREATE INDEX etl_dlq_source_open_idx
    ON etl_dlq (source, id)
    WHERE resolved_at IS NULL;

COMMENT ON TABLE etl_dlq IS
    'Dead-letter rows for ETL crawl runs that exhausted retries. One row per terminal failure; operators close them by setting resolved_at.';
COMMENT ON COLUMN etl_dlq.attempts IS
    'How many times the envelope tried before giving up. Includes the failing attempt — always ≥ 1.';
COMMENT ON COLUMN etl_dlq.payload IS
    'Connector-supplied context (cursor, HTTP status, response body excerpt, etc.). Schema-flexible JSONB — readers treat as opaque per row.';
