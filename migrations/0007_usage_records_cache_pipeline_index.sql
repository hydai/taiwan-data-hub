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
-- read `dataset_id` for the GROUP BY. Adding a partial index that
-- includes `dataset_id` in the index tuple lets the planner do the
-- aggregation index-only, keeping the tick cheap as the
-- usage_records table grows (~10k rows/day at v0.1 traffic; the
-- partial index keeps the size small by only covering
-- tool='query_rows' rows).

CREATE INDEX usage_records_query_rows_dataset_idx
    ON usage_records (dataset_id, requested_at DESC)
    WHERE tool = 'query_rows';

COMMENT ON INDEX usage_records_query_rows_dataset_idx IS
    'Covering index for #3.6 hot-cache pipeline candidate scans. Partial on tool=query_rows so the index is small.';
