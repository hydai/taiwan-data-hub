-- M5b.6: provenance & licensing metadata per dataset row.
--
-- Three new columns on `datasets`:
--
--   source_url   — URL of the upstream open-data hub (e.g.
--                  https://opendata.cwa.gov.tw). Distinct from
--                  `original_url`, which points at the specific
--                  dataset page. Nullable because data.gov.tw rows
--                  imported before this migration won't carry a
--                  back-stamp; the ETL upsert will fill it in on the
--                  next crawl.
--
--   license_url  — URL of the license document for the row's
--                  declared license (e.g. https://data.gov.tw/license).
--                  Nullable for the same reason as source_url; the
--                  /licenses page degrades gracefully when missing.
--
--   fetched_at   — when this row's metadata was last reconciled with
--                  upstream. Distinct from `last_modified_at` (the
--                  upstream's modification timestamp) and from
--                  `dataset_versions.fetched_at` (per-file). NOT NULL
--                  with a `now()` default so existing rows get a
--                  truthful-ish stamp at migration time; future upserts
--                  refresh it on every successful crawl.

ALTER TABLE datasets
    ADD COLUMN source_url  TEXT,
    ADD COLUMN license_url TEXT,
    ADD COLUMN fetched_at  TIMESTAMPTZ NOT NULL DEFAULT now();

-- Both URLs (when present) must be syntactically reasonable. The
-- connectors fully validate before insertion, but a CHECK keeps a
-- bad migration / manual UPDATE from corrupting the column.
ALTER TABLE datasets
    ADD CONSTRAINT datasets_source_url_scheme
        CHECK (source_url IS NULL
               OR source_url ~ '^https?://[^[:space:]]+$'),
    ADD CONSTRAINT datasets_license_url_scheme
        CHECK (license_url IS NULL
               OR license_url ~ '^https?://[^[:space:]]+$');
