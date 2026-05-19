//! `PostgreSQL` repositories (sqlx) and Parquet I/O (Polars).
//!
//! #1.4b ships the bare minimum needed by the upcoming ETL worker:
//! a [`Storage`] handle wrapping a `PgPool`, and
//! [`Storage::upsert_dataset`] which writes the connector-side
//! [`connectors::DatasetMetadata`] into the `datasets` table on the
//! `(source, source_id)` natural key. `domain_id` is passed in by
//! the caller — domain-mapping logic lives in the ETL layer
//! (#1.4c), not here, so the storage crate stays the "thin SQL"
//! layer the rest of the workspace depends on.
//!
//! Parquet IO and the remaining repositories (`dataset_versions`,
//! `dataset_files`, search) layer on top of this in later PRs.

mod dataset_repo;

pub use dataset_repo::{
    CacheRef, DatasetCacheLookup, DatasetFileRow, DatasetFull, DatasetKey, DatasetReader,
    DatasetRow, DatasetSearcher, DatasetVersionRow, DatasetWriter, SearchHit, SearchPage,
    SearchParams, SourceHttpState, Storage, StorageError, VersionWithFiles,
};
