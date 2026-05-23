//! Integration tests for [`auth::SubmissionService`] (#5a.1).
//!
//! Uses an in-memory [`storage::SubmissionRepo`] fake that
//! mirrors the production SQL semantics: rows land in
//! `pending`, `get_for_user` is ownership-scoped, `withdraw`
//! collapses the "not yours / not found / already terminal"
//! cases to `Ok(None)` so a probing attacker can't
//! distinguish.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use auth::{AuthError, SubmissionPayload, SubmissionService};
use chrono::{DateTime, Utc};
use serde_json::Value;
use storage::{
    NewSubmission, StorageError, SubmissionKind, SubmissionRepo, SubmissionRow, SubmissionStatus,
};
use uuid::Uuid;

#[derive(Default)]
struct InMemorySubmissionRepo {
    inner: Mutex<HashMap<Uuid, Row>>,
}

#[derive(Clone)]
struct Row {
    id: Uuid,
    user_id: Uuid,
    kind: SubmissionKind,
    status: SubmissionStatus,
    title: String,
    payload: Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    reviewed_at: Option<DateTime<Utc>>,
    reviewed_by: Option<Uuid>,
    review_reason: Option<String>,
}

#[async_trait]
impl SubmissionRepo for InMemorySubmissionRepo {
    async fn insert(&self, new: NewSubmission) -> Result<Uuid, StorageError> {
        let id = Uuid::now_v7();
        let row = Row {
            id,
            user_id: new.user_id,
            kind: new.kind,
            status: SubmissionStatus::Pending,
            title: new.title,
            payload: new.payload,
            created_at: new.created_at,
            updated_at: new.created_at,
            reviewed_at: None,
            reviewed_by: None,
            review_reason: None,
        };
        self.inner.lock().unwrap().insert(id, row);
        Ok(id)
    }

    async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<SubmissionRow>, StorageError> {
        let inner = self.inner.lock().unwrap();
        let mut rows: Vec<SubmissionRow> = inner
            .values()
            .filter(|r| r.user_id == user_id)
            .map(snapshot)
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        Ok(rows)
    }

    async fn get_for_user(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<SubmissionRow>, StorageError> {
        let inner = self.inner.lock().unwrap();
        Ok(inner
            .get(&id)
            .filter(|r| r.user_id == user_id)
            .map(snapshot))
    }

    async fn withdraw(
        &self,
        id: Uuid,
        user_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<Option<SubmissionRow>, StorageError> {
        let mut inner = self.inner.lock().unwrap();
        let Some(row) = inner.get_mut(&id) else {
            return Ok(None);
        };
        if row.user_id != user_id || row.status != SubmissionStatus::Pending {
            return Ok(None);
        }
        row.status = SubmissionStatus::Withdrawn;
        // Clamp via `max` so `updated_at` stays monotonic under
        // multi-instance clock skew — mirrors the production
        // `GREATEST(updated_at, $3)`.
        row.updated_at = row.updated_at.max(now);
        Ok(Some(snapshot(row)))
    }

    // Moderator-side methods (#5a.2). The author-side tests
    // in this file don't exercise them; #5a.2 has its own
    // dedicated moderation test module. We implement them as
    // unreachable to keep the trait satisfied without diluting
    // the focused author-side coverage.
    async fn list_pending(
        &self,
        _kind_filter: Option<SubmissionKind>,
    ) -> Result<Vec<SubmissionRow>, StorageError> {
        unreachable!("author-side submission tests do not exercise the moderation surface")
    }
    async fn get_for_moderation(&self, _id: Uuid) -> Result<Option<SubmissionRow>, StorageError> {
        unreachable!("author-side submission tests do not exercise the moderation surface")
    }
    async fn approve(
        &self,
        _id: Uuid,
        _mod_id: Uuid,
        _reason: Option<&str>,
        _now: DateTime<Utc>,
    ) -> Result<Option<SubmissionRow>, StorageError> {
        unreachable!("author-side submission tests do not exercise the moderation surface")
    }
    async fn reject(
        &self,
        _id: Uuid,
        _mod_id: Uuid,
        _reason: &str,
        _now: DateTime<Utc>,
    ) -> Result<Option<SubmissionRow>, StorageError> {
        unreachable!("author-side submission tests do not exercise the moderation surface")
    }
}

fn snapshot(row: &Row) -> SubmissionRow {
    SubmissionRow {
        id: row.id,
        user_id: row.user_id,
        kind: row.kind,
        status: row.status,
        title: row.title.clone(),
        payload: row.payload.clone(),
        created_at: row.created_at,
        updated_at: row.updated_at,
        reviewed_at: row.reviewed_at,
        reviewed_by: row.reviewed_by,
        review_reason: row.review_reason.clone(),
    }
}

fn build_service() -> SubmissionService {
    SubmissionService::new(Arc::new(InMemorySubmissionRepo::default()) as Arc<dyn SubmissionRepo>)
}

fn fresh_user_id() -> Uuid {
    Uuid::now_v7()
}

fn dataset_payload() -> SubmissionPayload {
    SubmissionPayload::Dataset {
        title: "Taiwan rainfall observations".into(),
        description: "Hourly observations from CWA stations.".into(),
        source_url: "https://example.gov.tw/rainfall.csv".into(),
        license: "CC-BY-4.0".into(),
        domain_slug: "weather-climate".into(),
    }
}

#[tokio::test]
async fn create_lands_in_pending_status() {
    let svc = build_service();
    let user = fresh_user_id();
    let id = svc
        .create(user, dataset_payload())
        .await
        .expect("create ok");
    let row = svc
        .get_for_user(id, user)
        .await
        .expect("get ok")
        .expect("Some");
    assert_eq!(row.status, SubmissionStatus::Pending);
    assert_eq!(row.kind, SubmissionKind::Dataset);
    assert!(row.payload["kind"].as_str() == Some("dataset"));
    assert!(row.payload["title"].as_str() == Some("Taiwan rainfall observations"));
    assert_eq!(row.title, "Taiwan rainfall observations");
}

#[tokio::test]
async fn validation_rejects_empty_required_field() {
    let svc = build_service();
    let user = fresh_user_id();
    let err = svc
        .create(
            user,
            SubmissionPayload::Tool {
                name: "  ".into(),
                description: "ok".into(),
                repo_url: "https://example.com".into(),
                language: "rust".into(),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::Validation(_)));
}

#[tokio::test]
async fn validation_rejects_non_http_url() {
    let svc = build_service();
    let user = fresh_user_id();
    let err = svc
        .create(
            user,
            SubmissionPayload::Connector {
                name: "tdh-connector".into(),
                description: "ok".into(),
                repo_url: "git+ssh://example.com".into(),
                license: "MIT".into(),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::Validation(_)));
}

#[tokio::test]
async fn list_for_user_is_owner_scoped() {
    let svc = build_service();
    let alice = fresh_user_id();
    let bob = fresh_user_id();
    let _ = svc.create(alice, dataset_payload()).await.unwrap();
    let _ = svc.create(alice, dataset_payload()).await.unwrap();
    let _ = svc.create(bob, dataset_payload()).await.unwrap();
    let alice_rows = svc.list_for_user(alice).await.unwrap();
    let bob_rows = svc.list_for_user(bob).await.unwrap();
    assert_eq!(alice_rows.len(), 2);
    assert_eq!(bob_rows.len(), 1);
}

#[tokio::test]
async fn get_for_user_404s_for_other_user() {
    let svc = build_service();
    let alice = fresh_user_id();
    let bob = fresh_user_id();
    let id = svc.create(alice, dataset_payload()).await.unwrap();
    assert!(svc.get_for_user(id, bob).await.unwrap().is_none());
    assert!(svc.get_for_user(id, alice).await.unwrap().is_some());
}

#[tokio::test]
async fn withdraw_flips_pending_to_withdrawn() {
    let svc = build_service();
    let user = fresh_user_id();
    let id = svc.create(user, dataset_payload()).await.unwrap();
    let withdrawn = svc.withdraw(id, user).await.unwrap().expect("Some");
    assert_eq!(withdrawn.status, SubmissionStatus::Withdrawn);
    // Idempotent: a second withdraw on the same id returns
    // None, matching the api-key revoke pattern.
    assert!(svc.withdraw(id, user).await.unwrap().is_none());
}

#[tokio::test]
async fn withdraw_is_owner_scoped() {
    let svc = build_service();
    let alice = fresh_user_id();
    let bob = fresh_user_id();
    let id = svc.create(alice, dataset_payload()).await.unwrap();
    assert!(svc.withdraw(id, bob).await.unwrap().is_none());
    let row = svc.get_for_user(id, alice).await.unwrap().unwrap();
    assert_eq!(row.status, SubmissionStatus::Pending);
}

#[tokio::test]
async fn playground_blank_repo_url_normalizes_to_none() {
    let svc = build_service();
    let user = fresh_user_id();
    let id = svc
        .create(
            user,
            SubmissionPayload::Playground {
                name: "weather-map".into(),
                description: "Live rainfall heatmap".into(),
                demo_url: "https://demo.example.com".into(),
                repo_url: Some("   ".into()),
            },
        )
        .await
        .expect("create ok");
    let row = svc.get_for_user(id, user).await.unwrap().unwrap();
    // The trimmed blank string serializes as `null`; serde maps
    // `Option<String>::None` to `null`, so the JSONB carries an
    // explicit null rather than an absent field.
    assert!(row.payload["repo_url"].is_null());
}
