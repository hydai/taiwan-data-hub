-- ── #3.6 hot-cache pipeline: covering index for candidate scans ──
--
-- The cache_pipeline tick (etl-worker, every 6 hours) runs two
-- GROUP BY queries over `usage_records`:
--   SELECT dataset_id, COUNT(*) ...
--   FROM usage_records
--   WHERE tool = 'query_rows' AND requested_at >= now() - interval
--   GROUP BY dataset_id;
--
-- The existing `usage_records_tool_requested_idx` (tool, requested_at
-- DESC) covers the WHERE clause but forces a heap fetch per row to
-- read `dataset_id` for the GROUP BY. We need an index whose leading
-- column is `requested_at` (so the range scan `requested_at >=
-- cutoff` is direct), with `dataset_id` available index-only for
-- the aggregation. The partial-on-tool predicate keeps it small
-- (matches the only query pattern that uses it).

CREATE INDEX usage_records_query_rows_window_idx
    ON usage_records (requested_at DESC)
    INCLUDE (dataset_id)
    WHERE tool = 'query_rows';

COMMENT ON INDEX usage_records_query_rows_window_idx IS
    'Covering index for #3.6 hot-cache pipeline: requested_at-leading range scan with dataset_id INCLUDE-only for index-only aggregation; partial on tool=query_rows.';
