-- M0 #0.8 — initial schema for the dataset catalog.
--
-- Covers the four core tables called out in docs/DESIGN.md §4.3:
--   domains            — 20-row enum-like table populated from config/domains.yaml
--   datasets           — per-dataset metadata (i18n title, tier, license, publisher, …)
--   dataset_versions   — schema history per dataset, captured by the ETL pipeline
--   dataset_files      — concrete files per version (CSV/JSON/Parquet/XML), with URI + checksum
--
-- Enums are encoded as TEXT + CHECK constraints (instead of CREATE TYPE)
-- to keep schema migrations simple — adding a new variant is a one-line
-- ALTER TABLE rather than a fresh enum type + cast dance.
--
-- Requires PostgreSQL ≥ 18 for the native `uuidv7()` function.

-- ── Domains ──────────────────────────────────────────────────────────
CREATE TABLE domains (
    id            SMALLSERIAL PRIMARY KEY,
    slug          TEXT NOT NULL UNIQUE,
    kind          TEXT NOT NULL CHECK (kind IN ('topical', 'meta', 'horizontal')),
    sort_order    INT  NOT NULL DEFAULT 0,
    name_i18n     JSONB NOT NULL,
    description_i18n JSONB,

    -- zh-TW is the project's source language; require it on every row.
    CONSTRAINT name_has_zh_tw CHECK (jsonb_typeof(name_i18n -> 'zh-TW') = 'string')
);

CREATE INDEX domains_kind_sort_idx ON domains (kind, sort_order);

COMMENT ON TABLE domains IS
    'Marketplace dataset domains — 20 rows, populated from config/domains.yaml via 0002_seed_domains.sql.';
COMMENT ON COLUMN domains.kind IS
    'topical (16) | meta (1) | horizontal (2) — controls section grouping on /data';
COMMENT ON COLUMN domains.name_i18n IS
    'JSONB shape {"zh-TW":"…","en":"…",…}; read with COALESCE(name_i18n->>$lang, name_i18n->>''zh-TW'')';

-- ── Datasets ─────────────────────────────────────────────────────────
CREATE TABLE datasets (
    id                  UUID PRIMARY KEY DEFAULT uuidv7(),
    source              TEXT NOT NULL CHECK (source IN (
                            'data_gov_tw', 'twse', 'moea', 'cwa', 'fishery_moa',
                            'user_contrib'
                        )),
    source_id           TEXT NOT NULL,
    slug                TEXT NOT NULL,
    domain_id           SMALLINT NOT NULL REFERENCES domains(id) ON DELETE RESTRICT,
    title_i18n          JSONB NOT NULL,
    description_i18n    JSONB,
    tier                TEXT NOT NULL DEFAULT 'bronze'
                        CHECK (tier IN ('platinum', 'gold', 'silver', 'bronze')),
    tier_override       TEXT
                        CHECK (tier_override IS NULL
                               OR tier_override IN ('platinum', 'gold', 'silver', 'bronze')),
    tier_score          NUMERIC(4, 3) NOT NULL DEFAULT 0
                        CHECK (tier_score >= 0 AND tier_score <= 1),
    license             TEXT NOT NULL,
    publisher           TEXT,
    update_frequency    TEXT,
    original_url        TEXT,
    schema_json         JSONB,
    row_count_estimate  BIGINT CHECK (row_count_estimate IS NULL OR row_count_estimate >= 0),
    cached              BOOLEAN NOT NULL DEFAULT FALSE,
    cache_path          TEXT,
    first_seen_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_modified_at    TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- Full-text search vector. M0 leaves the trigger that populates this
    -- for #1.5 (search_datasets MCP tool) — we only declare the column
    -- here so the GIN index can be created.
    tsv                 TSVECTOR,

    CONSTRAINT datasets_unique_per_source UNIQUE (source, source_id),
    CONSTRAINT title_has_zh_tw CHECK (jsonb_typeof(title_i18n -> 'zh-TW') = 'string')
);

CREATE INDEX datasets_domain_idx       ON datasets (domain_id);
CREATE INDEX datasets_tier_idx         ON datasets (tier) WHERE tier IN ('platinum', 'gold');
CREATE INDEX datasets_cached_idx       ON datasets (cached) WHERE cached;
CREATE INDEX datasets_tsv_idx          ON datasets USING GIN (tsv);
CREATE INDEX datasets_last_modified_idx ON datasets (last_modified_at DESC);

COMMENT ON COLUMN datasets.id IS
    'UUIDv7 (PG 18 native) — sortable by creation time without a separate timestamp column.';
COMMENT ON COLUMN datasets.tier IS
    'Auto-computed by the ETL tier classifier (see docs/DESIGN.md §4.5). tier_override wins when set.';
COMMENT ON COLUMN datasets.tsv IS
    'Full-text vector populated by a trigger added in M1 #1.5; GIN index already in place.';

-- ── Dataset versions ─────────────────────────────────────────────────
CREATE TABLE dataset_versions (
    id            UUID PRIMARY KEY DEFAULT uuidv7(),
    dataset_id    UUID NOT NULL REFERENCES datasets(id) ON DELETE CASCADE,
    version       TEXT NOT NULL,
    fetched_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    checksum      TEXT,
    row_count     BIGINT CHECK (row_count IS NULL OR row_count >= 0),
    schema_diff   JSONB,

    CONSTRAINT dataset_versions_unique UNIQUE (dataset_id, version)
);

CREATE INDEX dataset_versions_dataset_idx
    ON dataset_versions (dataset_id, fetched_at DESC);

COMMENT ON COLUMN dataset_versions.version IS
    'Upstream version string (or ETag / Last-Modified hash when none is provided).';
COMMENT ON COLUMN dataset_versions.schema_diff IS
    'Column add/drop/type-change diff between this and the previous version, emitted by the ETL schema-diff step.';

-- ── Dataset files ────────────────────────────────────────────────────
CREATE TABLE dataset_files (
    id                  UUID PRIMARY KEY DEFAULT uuidv7(),
    dataset_version_id  UUID NOT NULL REFERENCES dataset_versions(id) ON DELETE CASCADE,
    format              TEXT NOT NULL CHECK (format IN (
                            'csv', 'json', 'jsonl', 'parquet', 'xml', 'pdf', 'zip'
                        )),
    uri                 TEXT NOT NULL,
    byte_size           BIGINT CHECK (byte_size IS NULL OR byte_size >= 0),
    checksum            TEXT
);

CREATE INDEX dataset_files_version_idx
    ON dataset_files (dataset_version_id);

COMMENT ON COLUMN dataset_files.uri IS
    'Storage abstraction: file:// (local), s3:// (SeaweedFS/Garage), https:// (passthrough to upstream).';
