-- M1 #1.6a — make `datasets.slug` globally unique.
--
-- The marketplace UI uses `/data/<slug>` URLs (see docs/DESIGN.md
-- §1.2 and §4.4), which only make sense if slugs are unambiguous
-- across sources. The original 0001_init.sql declared only
-- `UNIQUE (source, source_id)`; this migration adds the global
-- slug constraint so storage::get_dataset(DatasetKey::Slug) is
-- deterministic.
--
-- sqlx_migrations tracks applied migrations, so the bare
-- ALTER TABLE below is run exactly once. Re-applying by hand
-- against a database that already has the constraint will fail
-- loudly — that's the desired behaviour.

ALTER TABLE datasets
    ADD CONSTRAINT datasets_slug_unique UNIQUE (slug);
