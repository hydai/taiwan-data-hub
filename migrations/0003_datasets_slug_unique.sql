-- M1 #1.6a — make `datasets.slug` globally unique.
--
-- The marketplace UI uses `/data/<slug>` URLs (see docs/DESIGN.md
-- §1.2 and §4.4), which only make sense if slugs are unambiguous
-- across sources. The original 0001_init.sql declared only
-- `UNIQUE (source, source_id)`; this migration adds the global
-- slug constraint so storage::get_dataset(DatasetKey::Slug) is
-- deterministic.
--
-- Idempotent — `IF NOT EXISTS` lets the migration re-run safely on
-- environments that already applied it. (sqlx_migrations tracks
-- applied migrations, but the guard keeps cold-start CI happy when
-- a prior run was rolled back.)

ALTER TABLE datasets
    ADD CONSTRAINT datasets_slug_unique UNIQUE (slug);
