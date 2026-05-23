//! Integration tests for [`auth::CommentService`] (#5a.3).
//!
//! Uses an in-memory [`storage::CommentRepo`] fake that
//! mirrors production semantics: depth ≤ 1, soft-delete
//! drops `body_md` to NULL, edit-window guard in the UPDATE
//! predicate.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use auth::{BodyError, CommentDenialReason, CommentService};
use chrono::{DateTime, Utc};
use storage::{CommentRepo, CommentRow, CommentTargetKind, NewComment, StorageError};
use uuid::Uuid;

#[derive(Default)]
struct CommentStore {
    rows: Mutex<HashMap<Uuid, Row>>,
}

#[derive(Clone)]
struct Row {
    id: Uuid,
    target_kind: CommentTargetKind,
    target_id: Uuid,
    parent_id: Option<Uuid>,
    user_id: Option<Uuid>,
    body_md: Option<String>,
    depth: i16,
    created_at: DateTime<Utc>,
    edited_at: Option<DateTime<Utc>>,
    deleted_at: Option<DateTime<Utc>>,
}

fn snapshot(row: &Row) -> CommentRow {
    CommentRow {
        id: row.id,
        target_kind: row.target_kind,
        target_id: row.target_id,
        parent_id: row.parent_id,
        user_id: row.user_id,
        body_md: row.body_md.clone(),
        depth: row.depth,
        created_at: row.created_at,
        edited_at: row.edited_at,
        deleted_at: row.deleted_at,
    }
}

#[async_trait]
impl CommentRepo for CommentStore {
    async fn insert(&self, new: NewComment) -> Result<Uuid, StorageError> {
        let id = Uuid::now_v7();
        self.rows.lock().unwrap().insert(
            id,
            Row {
                id,
                target_kind: new.target_kind,
                target_id: new.target_id,
                parent_id: new.parent_id,
                user_id: Some(new.user_id),
                body_md: Some(new.body_md),
                depth: new.depth,
                created_at: new.created_at,
                edited_at: None,
                deleted_at: None,
            },
        );
        Ok(id)
    }

    async fn get(&self, id: Uuid) -> Result<Option<CommentRow>, StorageError> {
        Ok(self.rows.lock().unwrap().get(&id).map(snapshot))
    }

    async fn list_for_target(
        &self,
        target_kind: CommentTargetKind,
        target_id: Uuid,
    ) -> Result<Vec<CommentRow>, StorageError> {
        let inner = self.rows.lock().unwrap();
        let mut rows: Vec<CommentRow> = inner
            .values()
            .filter(|r| r.target_kind == target_kind && r.target_id == target_id)
            .map(snapshot)
            .collect();
        rows.sort_by_key(|r| r.created_at);
        Ok(rows)
    }

    async fn edit(
        &self,
        id: Uuid,
        author_id: Uuid,
        new_body: &str,
        edit_window_secs: i64,
        now: DateTime<Utc>,
    ) -> Result<Option<CommentRow>, StorageError> {
        let mut inner = self.rows.lock().unwrap();
        let Some(row) = inner.get_mut(&id) else {
            return Ok(None);
        };
        if row.user_id != Some(author_id) || row.deleted_at.is_some() {
            return Ok(None);
        }
        // Mirror the production SQL's `make_interval(secs => …)`
        // comparison: whole-second resolution on the DB side.
        // The service rounds the window UP so a sub-second
        // configured window doesn't collapse to "0 seconds".
        let elapsed_secs = (now - row.created_at).num_seconds();
        if elapsed_secs < 0 || elapsed_secs > edit_window_secs {
            return Ok(None);
        }
        row.body_md = Some(new_body.to_owned());
        row.edited_at = Some(now);
        Ok(Some(snapshot(row)))
    }

    async fn delete(
        &self,
        id: Uuid,
        author_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<Option<CommentRow>, StorageError> {
        let mut inner = self.rows.lock().unwrap();
        let Some(row) = inner.get_mut(&id) else {
            return Ok(None);
        };
        if row.user_id != Some(author_id) || row.deleted_at.is_some() {
            return Ok(None);
        }
        row.deleted_at = Some(now);
        row.body_md = None;
        Ok(Some(snapshot(row)))
    }
}

fn build_service() -> CommentService {
    CommentService::new(Arc::new(CommentStore::default()) as Arc<dyn CommentRepo>)
}

fn dataset_target() -> (CommentTargetKind, Uuid) {
    (CommentTargetKind::Dataset, Uuid::now_v7())
}

#[tokio::test]
async fn create_root_then_list_renders_html() {
    let svc = build_service();
    let alice = Uuid::now_v7();
    let (kind, target) = dataset_target();
    let posted = svc
        .create(alice, kind, target, None, "**hello** world".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(posted.depth, 0);
    assert_eq!(posted.body_md.as_deref(), Some("**hello** world"));
    assert!(posted.body_html.contains("<strong>hello</strong>"));
    let listed = svc.list_for_target(kind, target).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, posted.id);
}

#[tokio::test]
async fn empty_body_rejected() {
    let svc = build_service();
    let user = Uuid::now_v7();
    let (kind, target) = dataset_target();
    let err = svc
        .create(user, kind, target, None, "   ".into())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err, CommentDenialReason::InvalidBody(BodyError::Empty));
}

#[tokio::test]
async fn reply_at_depth_1_is_allowed_but_depth_2_is_not() {
    let svc = build_service();
    let alice = Uuid::now_v7();
    let (kind, target) = dataset_target();
    let root = svc
        .create(alice, kind, target, None, "root".into())
        .await
        .unwrap()
        .unwrap();
    let reply = svc
        .create(alice, kind, target, Some(root.id), "reply".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reply.depth, 1);
    // Depth 2 — replying to a reply — must be refused.
    let err = svc
        .create(alice, kind, target, Some(reply.id), "deeper".into())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err, CommentDenialReason::DepthCapExceeded);
}

#[tokio::test]
async fn reply_with_mismatched_target_rejected() {
    let svc = build_service();
    let alice = Uuid::now_v7();
    let (kind, target_a) = dataset_target();
    let (_, target_b) = dataset_target();
    let root = svc
        .create(alice, kind, target_a, None, "on a".into())
        .await
        .unwrap()
        .unwrap();
    let err = svc
        .create(alice, kind, target_b, Some(root.id), "wrong target".into())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err, CommentDenialReason::ParentNotFound);
}

#[tokio::test]
async fn edit_within_window_updates_body() {
    let svc = build_service();
    let alice = Uuid::now_v7();
    let (kind, target) = dataset_target();
    let posted = svc
        .create(alice, kind, target, None, "first".into())
        .await
        .unwrap()
        .unwrap();
    let edited = svc
        .edit(alice, posted.id, "second".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(edited.body_md.as_deref(), Some("second"));
    assert!(edited.edited_at.is_some());
}

#[tokio::test]
async fn edit_past_window_returns_closed_denial() {
    // The edit-window check uses whole-second comparison, so
    // a sub-second window collapses to "0 seconds" and never
    // closes. Use 1 second + sleep ~1.2s to exercise the
    // boundary deterministically.
    let svc = build_service().with_edit_window(Duration::from_secs(1));
    let alice = Uuid::now_v7();
    let (kind, target) = dataset_target();
    let posted = svc
        .create(alice, kind, target, None, "first".into())
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let denial = svc
        .edit(alice, posted.id, "second".into())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(denial, CommentDenialReason::EditWindowClosed);
}

#[tokio::test]
async fn edit_by_other_user_404s() {
    let svc = build_service();
    let alice = Uuid::now_v7();
    let bob = Uuid::now_v7();
    let (kind, target) = dataset_target();
    let posted = svc
        .create(alice, kind, target, None, "alice's".into())
        .await
        .unwrap()
        .unwrap();
    let denial = svc
        .edit(bob, posted.id, "bob's".into())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(denial, CommentDenialReason::NotFoundOrNotYours);
}

#[tokio::test]
async fn delete_soft_drops_body_and_renders_tombstone() {
    let svc = build_service();
    let alice = Uuid::now_v7();
    let (kind, target) = dataset_target();
    let posted = svc
        .create(alice, kind, target, None, "to delete".into())
        .await
        .unwrap()
        .unwrap();
    let deleted = svc.delete(alice, posted.id).await.unwrap().unwrap();
    assert!(deleted.is_deleted);
    assert!(deleted.body_md.is_none());
    assert!(deleted.body_html.contains("[deleted]"));
    // Idempotent: second delete returns NotFoundOrNotYours.
    let denial = svc.delete(alice, posted.id).await.unwrap().unwrap_err();
    assert_eq!(denial, CommentDenialReason::NotFoundOrNotYours);
}

#[tokio::test]
async fn list_preserves_soft_deleted_rows_as_tombstones() {
    let svc = build_service();
    let alice = Uuid::now_v7();
    let (kind, target) = dataset_target();
    let parent = svc
        .create(alice, kind, target, None, "parent".into())
        .await
        .unwrap()
        .unwrap();
    let _reply = svc
        .create(alice, kind, target, Some(parent.id), "reply".into())
        .await
        .unwrap()
        .unwrap();
    let _ = svc.delete(alice, parent.id).await.unwrap().unwrap();
    // The reply is still visible — soft-delete preserves the
    // thread structure even when the parent's body is gone.
    let listed = svc.list_for_target(kind, target).await.unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().any(|c| c.is_deleted));
    assert!(listed.iter().any(|c| !c.is_deleted));
}

#[tokio::test]
async fn body_html_strips_unsafe_html() {
    let svc = build_service();
    let alice = Uuid::now_v7();
    let (kind, target) = dataset_target();
    // Two paragraphs so the surrounding "safe text" survives
    // regardless of how comrak suppresses the raw HTML block.
    let posted = svc
        .create(
            alice,
            kind,
            target,
            None,
            "safe text\n\n<script>alert('xss')</script>".into(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        !posted.body_html.contains("<script"),
        "html should not contain <script>; got {}",
        posted.body_html
    );
    assert!(
        posted.body_html.contains("safe text"),
        "html should keep the non-tag prose; got {}",
        posted.body_html
    );
}
