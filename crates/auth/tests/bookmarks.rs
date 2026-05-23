//! Integration tests for [`auth::BookmarkService`] +
//! [`auth::CollectionService`] (#5a.4).
//!
//! Uses in-memory [`storage::BookmarkRepo`] /
//! [`storage::CollectionRepo`] fakes that mirror production
//! semantics: idempotent heart toggle, ownership-scoped
//! collection ops, UNIQUE name per user.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use auth::{BookmarkService, CollectionDenialReason, CollectionService};
use chrono::{DateTime, Utc};
use storage::{
    BookmarkRepo, BookmarkRow, BookmarkTargetKind, BookmarkToggleOutcome, CollectionItemRow,
    CollectionRepo, CollectionRow, NewCollection, StorageError,
};
use uuid::Uuid;

// === Bookmark fake ===

#[derive(Default)]
struct BookmarkStore {
    rows: Mutex<HashMap<Uuid, BookmarkRow>>,
}

#[async_trait]
impl BookmarkRepo for BookmarkStore {
    async fn toggle(
        &self,
        user_id: Uuid,
        target_kind: BookmarkTargetKind,
        target_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<BookmarkToggleOutcome, StorageError> {
        let mut inner = self.rows.lock().unwrap();
        let existing = inner
            .iter()
            .find(|(_, r)| {
                r.user_id == user_id && r.target_kind == target_kind && r.target_id == target_id
            })
            .map(|(id, _)| *id);
        if let Some(id) = existing {
            inner.remove(&id);
            return Ok(BookmarkToggleOutcome::Removed);
        }
        let id = Uuid::now_v7();
        inner.insert(
            id,
            BookmarkRow {
                id,
                user_id,
                target_kind,
                target_id,
                created_at: now,
            },
        );
        Ok(BookmarkToggleOutcome::Bookmarked(id))
    }

    async fn list_for_user(
        &self,
        user_id: Uuid,
        kind_filter: Option<BookmarkTargetKind>,
    ) -> Result<Vec<BookmarkRow>, StorageError> {
        let inner = self.rows.lock().unwrap();
        let mut rows: Vec<BookmarkRow> = inner
            .values()
            .filter(|r| r.user_id == user_id && kind_filter.is_none_or(|k| r.target_kind == k))
            .cloned()
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        Ok(rows)
    }

    async fn which_bookmarked(
        &self,
        user_id: Uuid,
        targets: &[(BookmarkTargetKind, Uuid)],
    ) -> Result<Vec<(BookmarkTargetKind, Uuid)>, StorageError> {
        let inner = self.rows.lock().unwrap();
        let mut out = Vec::new();
        for (kind, id) in targets {
            if inner
                .values()
                .any(|r| r.user_id == user_id && r.target_kind == *kind && r.target_id == *id)
            {
                out.push((*kind, *id));
            }
        }
        Ok(out)
    }
}

// === Collection fake ===

#[derive(Default)]
struct CollectionStore {
    rows: Mutex<HashMap<Uuid, CollectionRow>>,
    items: Mutex<Vec<CollectionItemRow>>,
}

#[async_trait]
impl CollectionRepo for CollectionStore {
    async fn insert(&self, new: NewCollection) -> Result<CollectionRow, StorageError> {
        let mut inner = self.rows.lock().unwrap();
        if inner
            .values()
            .any(|r| r.user_id == new.user_id && r.name == new.name)
        {
            return Err(StorageError::UniqueViolation(
                "collections_unique_name_per_user".into(),
            ));
        }
        let id = Uuid::now_v7();
        let row = CollectionRow {
            id,
            user_id: new.user_id,
            name: new.name,
            description: new.description,
            created_at: new.created_at,
            updated_at: new.created_at,
        };
        inner.insert(id, row.clone());
        Ok(row)
    }

    async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<CollectionRow>, StorageError> {
        let inner = self.rows.lock().unwrap();
        let mut rows: Vec<CollectionRow> = inner
            .values()
            .filter(|r| r.user_id == user_id)
            .cloned()
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        Ok(rows)
    }

    async fn get_for_user(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<CollectionRow>, StorageError> {
        let inner = self.rows.lock().unwrap();
        Ok(inner.get(&id).filter(|r| r.user_id == user_id).cloned())
    }

    async fn update(
        &self,
        id: Uuid,
        user_id: Uuid,
        name: &str,
        description: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Option<CollectionRow>, StorageError> {
        let mut inner = self.rows.lock().unwrap();
        // Reject rename collisions before mutating.
        if inner
            .values()
            .any(|r| r.user_id == user_id && r.id != id && r.name == name)
        {
            return Err(StorageError::UniqueViolation(
                "collections_unique_name_per_user".into(),
            ));
        }
        let Some(row) = inner.get_mut(&id) else {
            return Ok(None);
        };
        if row.user_id != user_id {
            return Ok(None);
        }
        name.clone_into(&mut row.name);
        row.description = description.map(str::to_owned);
        row.updated_at = row.updated_at.max(now);
        Ok(Some(row.clone()))
    }

    async fn delete(&self, id: Uuid, user_id: Uuid) -> Result<bool, StorageError> {
        let mut inner = self.rows.lock().unwrap();
        if let Some(row) = inner.get(&id) {
            if row.user_id != user_id {
                return Ok(false);
            }
        } else {
            return Ok(false);
        }
        inner.remove(&id);
        // CASCADE the items.
        self.items.lock().unwrap().retain(|i| i.collection_id != id);
        Ok(true)
    }

    async fn list_items(
        &self,
        collection_id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<Vec<CollectionItemRow>>, StorageError> {
        let rows = self.rows.lock().unwrap();
        let owns = rows
            .get(&collection_id)
            .is_some_and(|r| r.user_id == user_id);
        if !owns {
            return Ok(None);
        }
        let items = self.items.lock().unwrap();
        let mut filtered: Vec<CollectionItemRow> = items
            .iter()
            .filter(|i| i.collection_id == collection_id)
            .cloned()
            .collect();
        filtered.sort_by_key(|i| std::cmp::Reverse(i.added_at));
        Ok(Some(filtered))
    }

    async fn add_item(
        &self,
        collection_id: Uuid,
        user_id: Uuid,
        target_kind: BookmarkTargetKind,
        target_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<bool, StorageError> {
        let rows = self.rows.lock().unwrap();
        let owns = rows
            .get(&collection_id)
            .is_some_and(|r| r.user_id == user_id);
        if !owns {
            return Ok(false);
        }
        drop(rows);
        let mut items = self.items.lock().unwrap();
        if items.iter().any(|i| {
            i.collection_id == collection_id
                && i.target_kind == target_kind
                && i.target_id == target_id
        }) {
            // Composite PK absorbs the dup.
            return Ok(false);
        }
        items.push(CollectionItemRow {
            collection_id,
            target_kind,
            target_id,
            added_at: now,
        });
        Ok(true)
    }

    async fn remove_item(
        &self,
        collection_id: Uuid,
        user_id: Uuid,
        target_kind: BookmarkTargetKind,
        target_id: Uuid,
    ) -> Result<bool, StorageError> {
        let rows = self.rows.lock().unwrap();
        let owns = rows
            .get(&collection_id)
            .is_some_and(|r| r.user_id == user_id);
        if !owns {
            return Ok(false);
        }
        drop(rows);
        let mut items = self.items.lock().unwrap();
        let before = items.len();
        items.retain(|i| {
            !(i.collection_id == collection_id
                && i.target_kind == target_kind
                && i.target_id == target_id)
        });
        Ok(items.len() < before)
    }
}

// === Helpers ===

fn build_bookmark_service() -> BookmarkService {
    BookmarkService::new(Arc::new(BookmarkStore::default()) as Arc<dyn BookmarkRepo>)
}

fn build_collection_service() -> CollectionService {
    CollectionService::new(Arc::new(CollectionStore::default()) as Arc<dyn CollectionRepo>)
}

// === Bookmark tests ===

#[tokio::test]
async fn heart_toggle_round_trips() {
    let svc = build_bookmark_service();
    let user = Uuid::now_v7();
    let target = Uuid::now_v7();
    let first = svc
        .toggle(user, BookmarkTargetKind::Dataset, target)
        .await
        .unwrap();
    assert!(matches!(first, BookmarkToggleOutcome::Bookmarked(_)));
    let second = svc
        .toggle(user, BookmarkTargetKind::Dataset, target)
        .await
        .unwrap();
    assert_eq!(second, BookmarkToggleOutcome::Removed);
    let third = svc
        .toggle(user, BookmarkTargetKind::Dataset, target)
        .await
        .unwrap();
    assert!(matches!(third, BookmarkToggleOutcome::Bookmarked(_)));
}

#[tokio::test]
async fn list_kind_filter_is_applied() {
    let svc = build_bookmark_service();
    let user = Uuid::now_v7();
    let d = Uuid::now_v7();
    let t = Uuid::now_v7();
    let _ = svc
        .toggle(user, BookmarkTargetKind::Dataset, d)
        .await
        .unwrap();
    let _ = svc.toggle(user, BookmarkTargetKind::Tool, t).await.unwrap();
    let datasets = svc
        .list_for_user(user, Some(BookmarkTargetKind::Dataset))
        .await
        .unwrap();
    assert_eq!(datasets.len(), 1);
    assert_eq!(datasets[0].target_id, d);
    let all = svc.list_for_user(user, None).await.unwrap();
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn which_bookmarked_returns_only_matches() {
    let svc = build_bookmark_service();
    let user = Uuid::now_v7();
    let d1 = Uuid::now_v7();
    let d2 = Uuid::now_v7();
    let d3 = Uuid::now_v7();
    let _ = svc
        .toggle(user, BookmarkTargetKind::Dataset, d1)
        .await
        .unwrap();
    let _ = svc
        .toggle(user, BookmarkTargetKind::Dataset, d3)
        .await
        .unwrap();
    let pairs = vec![
        (BookmarkTargetKind::Dataset, d1),
        (BookmarkTargetKind::Dataset, d2),
        (BookmarkTargetKind::Dataset, d3),
    ];
    let result = svc.which_bookmarked(user, &pairs).await.unwrap();
    assert_eq!(result.len(), 2);
    assert!(result.contains(&(BookmarkTargetKind::Dataset, d1)));
    assert!(result.contains(&(BookmarkTargetKind::Dataset, d3)));
}

// === Collection tests ===

#[tokio::test]
async fn collection_create_then_list() {
    let svc = build_collection_service();
    let user = Uuid::now_v7();
    let outcome = svc
        .create(user, "favorites".into(), Some("the best".into()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome.name, "favorites");
    let list = svc.list_for_user(user).await.unwrap();
    assert_eq!(list.len(), 1);
}

#[tokio::test]
async fn duplicate_collection_name_is_409() {
    let svc = build_collection_service();
    let user = Uuid::now_v7();
    let _ = svc.create(user, "f".into(), None).await.unwrap().unwrap();
    let denial = svc
        .create(user, "f".into(), None)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(denial, CollectionDenialReason::NameTaken);
}

#[tokio::test]
async fn rename_to_collision_returns_409() {
    let svc = build_collection_service();
    let user = Uuid::now_v7();
    let a = svc.create(user, "a".into(), None).await.unwrap().unwrap();
    let _b = svc.create(user, "b".into(), None).await.unwrap().unwrap();
    let denial = svc
        .rename(a.id, user, "b".into(), None)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(denial, CollectionDenialReason::NameTaken);
}

#[tokio::test]
async fn add_and_list_items() {
    let svc = build_collection_service();
    let user = Uuid::now_v7();
    let col = svc
        .create(user, "starred".into(), None)
        .await
        .unwrap()
        .unwrap();
    let target = Uuid::now_v7();
    let added = svc
        .add_item(col.id, user, BookmarkTargetKind::Dataset, target)
        .await
        .unwrap();
    assert!(added);
    // Second add is idempotent.
    let again = svc
        .add_item(col.id, user, BookmarkTargetKind::Dataset, target)
        .await
        .unwrap();
    assert!(!again);
    let items = svc.list_items(col.id, user).await.unwrap().unwrap();
    assert_eq!(items.len(), 1);
}

#[tokio::test]
async fn list_items_for_non_owner_returns_none() {
    let svc = build_collection_service();
    let alice = Uuid::now_v7();
    let bob = Uuid::now_v7();
    let col = svc.create(alice, "x".into(), None).await.unwrap().unwrap();
    let res = svc.list_items(col.id, bob).await.unwrap();
    assert!(res.is_none());
}

#[tokio::test]
async fn empty_name_rejected() {
    let svc = build_collection_service();
    let user = Uuid::now_v7();
    let denial = svc
        .create(user, "   ".into(), None)
        .await
        .unwrap()
        .unwrap_err();
    assert!(matches!(denial, CollectionDenialReason::InvalidInput(_)));
}
