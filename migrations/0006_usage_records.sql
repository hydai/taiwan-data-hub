-- ── #1.8 usage_records — per-call MCP usage audit ───────────────────
--
-- One row per MCP tool invocation that yields a billable / auditable
-- artefact. Initially only `materialize_dataset` writes here (the
-- other 4 base tools are cheap reads and don't audit at this
-- granularity); future tools call the same writer.
--
-- DESIGN.md §4.3 calls for monthly partitioning to keep abuse-
-- detection queries fast, but partition management adds operational
-- weight we don't need at pre-alpha row counts. The schema is
-- partition-ready (a single `requested_at` PARTITION BY RANGE wrap
-- ships in a follow-up issue) and queries are written to use only
-- the columns that would remain after the wrap.
--
-- `principal_kind` carries the auth surface so post-#4 (auth +
-- multi-user mode) can join into `users` / `mcp_api_keys` without a
-- schema change — until those tables exist we just store the kind
-- and the opaque id.

CREATE TABLE usage_records (
    id                  UUID PRIMARY KEY DEFAULT uuidv7(),
    dataset_id          UUID NOT NULL REFERENCES datasets(id) ON DELETE CASCADE,
    dataset_version_id  UUID REFERENCES dataset_versions(id) ON DELETE SET NULL,
    tool                TEXT NOT NULL CHECK (tool IN (
                            'list_domains', 'search_datasets', 'get_dataset',
                            'query_rows', 'materialize_dataset'
                        )),
    format              TEXT CHECK (format IS NULL OR format IN (
                            'csv', 'json', 'jsonl', 'parquet', 'xml', 'pdf', 'zip'
                        )),
    principal_kind      TEXT NOT NULL CHECK (principal_kind IN (
                            'anonymous', 'user', 'api_key'
                        )),
    principal_id        TEXT,
    byte_size           BIGINT CHECK (byte_size IS NULL OR byte_size >= 0),
    requested_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX usage_records_dataset_requested_idx
    ON usage_records (dataset_id, requested_at DESC);

CREATE INDEX usage_records_tool_requested_idx
    ON usage_records (tool, requested_at DESC);

COMMENT ON TABLE usage_records IS
    'Per-call MCP tool audit. Written by every tool that produces a billable artefact (#1.8 materialize_dataset; future tools).';
COMMENT ON COLUMN usage_records.principal_kind IS
    'Auth surface: anonymous (personal mode / no auth), user (session), api_key (mcp_api_keys row).';
COMMENT ON COLUMN usage_records.principal_id IS
    'Opaque caller identifier. NULL for anonymous; user UUID for sessions; api_key hash prefix for keys (NEVER the full secret).';
COMMENT ON COLUMN usage_records.byte_size IS
    'Bytes the caller is authorised to fetch. For materialize_dataset this is the underlying file size at presign time.';
