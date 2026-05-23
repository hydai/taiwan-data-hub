//! Bookmark + collection service (#5a.4).
//!
//! Validation + service-layer business logic on top of
//! [`storage::BookmarkRepo`] / [`storage::CollectionRepo`].
//!
//! Surface delivered:
//!
//!   * `BookmarkService::toggle` — heart on/off, idempotent.
//!   * `BookmarkService::list_for_user` — "my bookmarks",
//!     optionally filtered by kind.
//!   * `BookmarkService::which_bookmarked` — bulk decoration
//!     for catalog list pages (one query, N cards).
//!   * `CollectionService` — create / list / rename / delete
//!     private collections + add / remove items.

use std::sync::Arc;

use chrono::Utc;
use storage::{
    BookmarkRepo, BookmarkRow, BookmarkTargetKind, BookmarkToggleOutcome, CollectionItemRow,
    CollectionRepo, CollectionRow, NewCollection, StorageError,
};
use uuid::Uuid;

use crate::error::AuthError;

/// Display-name cap for a collection. Lets the sidebar list
/// without truncation while still bounding what a user can
/// type.
pub const COLLECTION_NAME_MAX_LEN: usize = 80;

/// Description cap. Optional field; same envelope as a
/// submission description.
pub const COLLECTION_DESCRIPTION_MAX_LEN: usize = 2048;

/// Why a collection write was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionDenialReason {
    /// Caller is not the owner, or the id doesn't exist.
    /// Folded so a probing attacker can't enumerate.
    NotFoundOrNotYours,
    /// `name` already taken for this user. The UI surfaces a
    /// "pick a different name" hint.
    NameTaken,
    /// `name` was empty after trim, or `description` exceeded
    /// the cap.
    InvalidInput(InputError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputError {
    NameEmpty,
    NameTooLong,
    DescriptionTooLong,
}

#[derive(Clone)]
pub struct BookmarkService {
    repo: Arc<dyn BookmarkRepo>,
}

impl std::fmt::Debug for BookmarkService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BookmarkService").finish_non_exhaustive()
    }
}

impl BookmarkService {
    #[must_use]
    pub fn new(repo: Arc<dyn BookmarkRepo>) -> Self {
        Self { repo }
    }

    pub async fn toggle(
        &self,
        user_id: Uuid,
        target_kind: BookmarkTargetKind,
        target_id: Uuid,
    ) -> Result<BookmarkToggleOutcome, AuthError> {
        Ok(self
            .repo
            .toggle(user_id, target_kind, target_id, Utc::now())
            .await?)
    }

    pub async fn list_for_user(
        &self,
        user_id: Uuid,
        kind_filter: Option<BookmarkTargetKind>,
    ) -> Result<Vec<BookmarkRow>, AuthError> {
        Ok(self.repo.list_for_user(user_id, kind_filter).await?)
    }

    pub async fn which_bookmarked(
        &self,
        user_id: Uuid,
        targets: &[(BookmarkTargetKind, Uuid)],
    ) -> Result<Vec<(BookmarkTargetKind, Uuid)>, AuthError> {
        Ok(self.repo.which_bookmarked(user_id, targets).await?)
    }
}

#[derive(Clone)]
pub struct CollectionService {
    repo: Arc<dyn CollectionRepo>,
}

impl std::fmt::Debug for CollectionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CollectionService").finish_non_exhaustive()
    }
}

impl CollectionService {
    #[must_use]
    pub fn new(repo: Arc<dyn CollectionRepo>) -> Self {
        Self { repo }
    }

    pub async fn create(
        &self,
        user_id: Uuid,
        name: String,
        description: Option<String>,
    ) -> Result<Result<CollectionRow, CollectionDenialReason>, AuthError> {
        let (name, description) = match validate_inputs(&name, description.as_deref()) {
            Ok(parts) => parts,
            Err(err) => return Ok(Err(CollectionDenialReason::InvalidInput(err))),
        };
        let outcome = self
            .repo
            .insert(NewCollection {
                user_id,
                name,
                description,
                created_at: Utc::now(),
            })
            .await;
        match outcome {
            Ok(row) => Ok(Ok(row)),
            Err(StorageError::UniqueViolation(_)) => Ok(Err(CollectionDenialReason::NameTaken)),
            Err(other) => Err(other.into()),
        }
    }

    pub async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<CollectionRow>, AuthError> {
        Ok(self.repo.list_for_user(user_id).await?)
    }

    pub async fn get_for_user(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<CollectionRow>, AuthError> {
        Ok(self.repo.get_for_user(id, user_id).await?)
    }

    /// Rename a collection and optionally update its
    /// description. `description` follows PATCH semantics:
    ///   * `None` → preserve the prior value.
    ///   * `Some(None)` → clear the column.
    ///   * `Some(Some(s))` → replace.
    ///
    /// The `Option<Option<T>>` shape is the canonical serde
    /// pattern for three-state PATCH semantics; clippy's
    /// `option_option` lint flags it by default because the
    /// shape is usually a mistake — here it's a deliberate
    /// protocol detail.
    #[allow(clippy::option_option)]
    pub async fn rename(
        &self,
        id: Uuid,
        user_id: Uuid,
        name: String,
        description: Option<Option<String>>,
    ) -> Result<Result<CollectionRow, CollectionDenialReason>, AuthError> {
        // Validate name + (when present) the new description.
        // `description = None` (preserve) skips description
        // validation entirely.
        let name_check_input = description.as_ref().and_then(|d| d.as_deref());
        let (name, validated_desc) = match validate_inputs(&name, name_check_input) {
            Ok(parts) => parts,
            Err(err) => return Ok(Err(CollectionDenialReason::InvalidInput(err))),
        };
        let description_arg: Option<Option<&str>> =
            description.as_ref().map(|_| validated_desc.as_deref());
        let outcome = self
            .repo
            .update(id, user_id, &name, description_arg, Utc::now())
            .await;
        match outcome {
            Ok(Some(row)) => Ok(Ok(row)),
            Ok(None) => Ok(Err(CollectionDenialReason::NotFoundOrNotYours)),
            Err(StorageError::UniqueViolation(_)) => Ok(Err(CollectionDenialReason::NameTaken)),
            Err(other) => Err(other.into()),
        }
    }

    pub async fn delete(&self, id: Uuid, user_id: Uuid) -> Result<bool, AuthError> {
        Ok(self.repo.delete(id, user_id).await?)
    }

    pub async fn list_items(
        &self,
        collection_id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<Vec<CollectionItemRow>>, AuthError> {
        Ok(self.repo.list_items(collection_id, user_id).await?)
    }

    pub async fn add_item(
        &self,
        collection_id: Uuid,
        user_id: Uuid,
        target_kind: BookmarkTargetKind,
        target_id: Uuid,
    ) -> Result<bool, AuthError> {
        Ok(self
            .repo
            .add_item(collection_id, user_id, target_kind, target_id, Utc::now())
            .await?)
    }

    pub async fn remove_item(
        &self,
        collection_id: Uuid,
        user_id: Uuid,
        target_kind: BookmarkTargetKind,
        target_id: Uuid,
    ) -> Result<bool, AuthError> {
        Ok(self
            .repo
            .remove_item(collection_id, user_id, target_kind, target_id)
            .await?)
    }
}

fn validate_inputs(
    name: &str,
    description: Option<&str>,
) -> Result<(String, Option<String>), InputError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(InputError::NameEmpty);
    }
    if name.chars().count() > COLLECTION_NAME_MAX_LEN {
        return Err(InputError::NameTooLong);
    }
    let description = description.map(str::trim);
    let description = if matches!(description, Some("")) {
        None
    } else {
        description
    };
    if let Some(d) = description {
        if d.chars().count() > COLLECTION_DESCRIPTION_MAX_LEN {
            return Err(InputError::DescriptionTooLong);
        }
    }
    Ok((name.to_owned(), description.map(str::to_owned)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_name_rejected() {
        assert_eq!(
            validate_inputs("  ", None).unwrap_err(),
            InputError::NameEmpty
        );
    }

    #[test]
    fn long_name_rejected() {
        let too_long: String = "x".repeat(COLLECTION_NAME_MAX_LEN + 1);
        assert_eq!(
            validate_inputs(&too_long, None).unwrap_err(),
            InputError::NameTooLong
        );
    }

    #[test]
    fn blank_description_normalizes_to_none() {
        let (_, desc) = validate_inputs("ok", Some("   ")).unwrap();
        assert!(desc.is_none());
    }
}
