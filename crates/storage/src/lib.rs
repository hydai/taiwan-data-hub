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

mod api_key_repo;
mod audit_repo;
mod auth_repo;
mod bookmark_repo;
mod comment_repo;
mod dataset_repo;
mod oauth_repo;
mod rate_limit_repo;
mod rating_repo;
mod report_repo;
mod session_repo;
mod sqlx_errors;
mod submission_repo;

pub use api_key_repo::{ApiKeyRepo, ApiKeyRow, NewApiKey};
pub use audit_repo::{AuditAction, AuditLogRepo, AuditTargetKind, NewAuditLog};
pub use auth_repo::{AuthTokenKind, AuthTokenRepo, User, UserRepo, UserRole};
pub use bookmark_repo::{
    BookmarkRepo, BookmarkRow, BookmarkTargetKind, BookmarkToggleOutcome, CollectionItemRow,
    CollectionRepo, CollectionRow, NewCollection,
};
pub use comment_repo::{CommentRepo, CommentRow, CommentTargetKind, NewComment};
pub use dataset_repo::{
    CacheCandidate, CacheHitRatio, CacheRef, CacheState, DatasetCacheLookup, DatasetFileRow,
    DatasetFull, DatasetKey, DatasetLatestFiles, DatasetReader, DatasetRow, DatasetSearcher,
    DatasetVersionRow, DatasetWriter, MaterializeView, NewUsageRecord, SearchHit, SearchPage,
    SearchParams, SourceHttpState, Storage, StorageError, UsageRecorder, VersionWithFiles,
};
pub use oauth_repo::{NewOAuthAccount, OAuthAccountRepo, OAuthPendingState, OAuthStateRepo};
pub use rate_limit_repo::{CounterTick, RateLimitRepo};
pub use rating_repo::{RatingAggregateRow, RatingRepo, RatingRow, RatingTargetKind, RatingView};
pub use report_repo::{
    InsertOutcome as ReportInsertOutcome, NewReport, ReportAction, ReportReason, ReportRepo,
    ReportRow, ReportTargetKind, ResolveSpec,
};
pub use session_repo::{AuthenticatedSession, NewSession, SessionRepo};
pub use submission_repo::{
    NewSubmission, SubmissionKind, SubmissionRepo, SubmissionRow, SubmissionStatus,
};
