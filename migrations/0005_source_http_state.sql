-- ── #1.4d.2 source_http_state — per-source ETag / Last-Modified ──────
--
-- Adds the small bookkeeping table the ETL driver needs to send
-- conditional `If-None-Match` / `If-Modified-Since` headers on the
-- first request of each crawl. When upstream returns `304 Not
-- Modified` the driver skips the rest of the pagination walk and
-- records a fresh `last_seen_at` instead of re-ingesting unchanged
-- metadata.
--
-- One row per upstream source — the natural key here is the source
-- enum we already use in `datasets.source`. We keep the value
-- domain in sync with the same CHECK constraint so a new connector
-- can't silently insert a value the rest of the schema doesn't
-- recognise.
--
-- Both `etag` and `last_modified` are nullable: the upstream server
-- may emit only one (or neither), and on first crawl neither has
-- been observed yet. `last_seen_at` always updates on a successful
-- crawl (regardless of 200 vs 304) so operators can answer "when
-- did the ETL last successfully talk to source X?" by reading one
-- column.

CREATE TABLE source_http_state (
    source         TEXT PRIMARY KEY CHECK (source IN (
                       'data_gov_tw', 'twse', 'moea', 'cwa', 'fishery_moa',
                       'user_contrib'
                   )),
    etag           TEXT,
    last_modified  TEXT,
    last_seen_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

COMMENT ON TABLE source_http_state IS
    'Per-source HTTP cache state for ETL conditional fetch (#1.4d.2). One row per upstream source.';
COMMENT ON COLUMN source_http_state.etag IS
    'Latest ETag observed on the catalog list endpoint. Sent back as If-None-Match on the next crawl.';
COMMENT ON COLUMN source_http_state.last_modified IS
    'Latest Last-Modified header observed on the catalog list endpoint. Sent back as If-Modified-Since.';
COMMENT ON COLUMN source_http_state.last_seen_at IS
    'Wall-clock timestamp of the last successful crawl (200 OR 304). Updated by the ETL driver.';
