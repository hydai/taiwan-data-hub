-- ── #1.5 search_datasets — FTS trigger + trigram backup ─────────────────
--
-- Two indexes work together so search behaves sensibly across English
-- and Chinese, without requiring an out-of-tree extension (zhparser
-- needs a custom Postgres image; we want `git clone && docker compose
-- up` to give working search out of the box).
--
--   1. `datasets.tsv` (already declared in 0001_init.sql, GIN-indexed)
--      Populated by a trigger from title / description / publisher,
--      using PG's bundled `simple` text-search config. `simple` does
--      not segment CJK (a Chinese title becomes one big token), so
--      `tsv @@ plainto_tsquery('simple', '土地')` would miss
--      "土地利用現況". That's where trigram comes in.
--
--   2. `datasets.searchable_text` (added below)
--      Generated stored column concatenating the same fields, with a
--      GIN `gin_trgm_ops` index. Trigrams work uniformly on UTF-8
--      bytes so CJK substring search (`ILIKE '%土地%'`) is fast.
--
-- `search_datasets` uses `tsv @@ ... OR searchable_text ILIKE ...`
-- so the planner picks whichever index hits. zhparser remains a
-- recommended production add-on (DESIGN.md §4.3) but is not required
-- for correctness — a follow-up issue tracks the packaging story.

-- pg_trgm ships with stock Postgres (including postgres:18-alpine),
-- so this is safe in the same migration as the trigger.
CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- ── tsv trigger ──────────────────────────────────────────────────────
--
-- Weighting matches typical FTS conventions:
--   A: title (most authoritative)
--   B: description (more context but lower density)
--   C: publisher (helps when users search for an agency)
-- ts_rank uses these weights when ranking results.
CREATE OR REPLACE FUNCTION datasets_tsv_update() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    NEW.tsv :=
        setweight(to_tsvector('simple', coalesce(NEW.title_i18n->>'zh-TW', '')), 'A') ||
        setweight(to_tsvector('simple', coalesce(NEW.title_i18n->>'en', '')), 'A') ||
        setweight(to_tsvector('simple', coalesce(NEW.description_i18n->>'zh-TW', '')), 'B') ||
        setweight(to_tsvector('simple', coalesce(NEW.description_i18n->>'en', '')), 'B') ||
        setweight(to_tsvector('simple', coalesce(NEW.publisher, '')), 'C');
    RETURN NEW;
END;
$$;

-- BEFORE INSERT OR UPDATE: trigger fires on every write so tsv stays
-- consistent. `OF` clauses would skip rewrites that touch only other
-- columns, which is fine and saves CPU. We watch the three columns
-- the trigger reads.
CREATE TRIGGER datasets_tsv_trigger
BEFORE INSERT OR UPDATE OF title_i18n, description_i18n, publisher ON datasets
FOR EACH ROW EXECUTE FUNCTION datasets_tsv_update();

-- ── searchable_text generated column + trigram index ────────────────
--
-- STORED (not VIRTUAL) so the GIN index can be built on it. We use
-- `coalesce(... , '')` so a missing locale or null publisher doesn't
-- propagate NULL across the concat.
ALTER TABLE datasets ADD COLUMN searchable_text TEXT
    GENERATED ALWAYS AS (
        coalesce(title_i18n->>'zh-TW', '') || ' ' ||
        coalesce(title_i18n->>'en', '') || ' ' ||
        coalesce(description_i18n->>'zh-TW', '') || ' ' ||
        coalesce(description_i18n->>'en', '') || ' ' ||
        coalesce(publisher, '')
    ) STORED;

CREATE INDEX datasets_searchable_trgm_idx ON datasets USING GIN (searchable_text gin_trgm_ops);

COMMENT ON COLUMN datasets.searchable_text IS
    'Concatenated title/description/publisher for trigram substring search (CJK-friendly fallback to tsv).';

-- ── Backfill ─────────────────────────────────────────────────────────
--
-- The trigger only fires on INSERT/UPDATE of the watched columns, so
-- existing rows still have tsv = NULL. Force re-evaluation with a
-- no-op self-assignment of the trigger's input. (`searchable_text` is
-- a generated column so it backfills automatically.)
UPDATE datasets SET title_i18n = title_i18n;
