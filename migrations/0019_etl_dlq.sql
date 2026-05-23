-- #5b.1 etl_dlq — dead-letter table for ETL crawl runs that
-- exhausted their retry budget.
--
-- One row per terminal failure. The retry-with-backoff
-- envelope (etl-worker/src/retry.rs) writes here AFTER it
-- gives up on a connector call — transient transport
-- errors that resolved on a later attempt never land in
-- the DLQ. Operators read the DLQ to find sources that
-- need manual attention; rows stay until the operator
-- explicitly sets `resolved_at`, so the DLQ doubles as an
-- audit trail.
--
-- `error_kind` is denormalised from `ConnectorError` so
-- the SQL queries can `WHERE error_kind = 'bad_status'`
-- without parsing the message. The CHECK constraint
-- pins the set; extending the enum is a service + CHECK
-- update.
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
    -- A coarse operation tag — `list_datasets`,
    -- `fetch_metadata`, `fetch_data`, etc. Lets the
    -- operator query "all DLQ rows from the catalog walk
    -- vs. the per-dataset fetch" without parsing the
    -- payload. Free-form by design — adding a new
    -- operation kind shouldn't require a migration.
    job_kind      TEXT         NOT NULL,
    -- Total attempts the envelope made before giving up,
    -- including the failing one. Always ≥ 1.
    attempts      INTEGER      NOT NULL CHECK (attempts >= 1),
    -- Normalised error category. Mirrors
    -- `connectors::ConnectorError` variant names so the
    -- envelope can map without lossy stringification.
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
-- first". Partial predicate matches the query exactly so
-- the scan stays bounded as resolved rows accumulate.
CREATE INDEX etl_dlq_open_idx
    ON etl_dlq (created_at DESC)
    WHERE resolved_at IS NULL;

-- Per-source health view — "how many open DLQ rows does
-- this source have?". Drives the operator dashboard's
-- "sources needing attention" panel.
CREATE INDEX etl_dlq_source_open_idx
    ON etl_dlq (source, created_at DESC)
    WHERE resolved_at IS NULL;

COMMENT ON TABLE etl_dlq IS
    'Dead-letter rows for ETL crawl runs that exhausted retries. One row per terminal failure; operators close them by setting resolved_at.';
COMMENT ON COLUMN etl_dlq.attempts IS
    'How many times the envelope tried before giving up. Includes the failing attempt — always ≥ 1.';
COMMENT ON COLUMN etl_dlq.payload IS
    'Connector-supplied context (cursor, HTTP status, response body excerpt, etc.). Schema-flexible JSONB — readers treat as opaque per row.';
