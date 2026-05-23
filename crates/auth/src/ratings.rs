//! Rating service (#5a.5).
//!
//! Wraps [`storage::RatingRepo`] with:
//!
//!   * score range validation (1..=5);
//!   * a 24h account-age anti-spam gate enforced at the
//!     service layer (rather than via a CHECK) so the
//!     threshold can move without a schema migration;
//!   * domain-typed denial reasons the HTTP layer maps to
//!     `4xx` status codes.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use storage::{RatingRepo, RatingRow, RatingTargetKind, RatingView, UserRepo};
use uuid::Uuid;

use crate::error::AuthError;

/// Minimum account age before a user can submit their first
/// rating. Matches the `DoD` on issue #53.
pub const MIN_ACCOUNT_AGE_FOR_RATING: Duration = Duration::hours(24);

/// Allowed score range (inclusive). Mirrored at the SQL
/// CHECK so a future caller bypassing the service can't
/// land out-of-band data.
pub const SCORE_MIN: i16 = 1;
pub const SCORE_MAX: i16 = 5;

/// Why a rating write was denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RatingDenialReason {
    /// Score wasn't in `[SCORE_MIN, SCORE_MAX]`. Gateway → 400.
    ScoreOutOfRange,
    /// Caller's account is younger than
    /// [`MIN_ACCOUNT_AGE_FOR_RATING`]. Gateway → 403 with a
    /// "wait until your account is older" hint.
    AccountTooNew,
    /// Caller's `user_id` doesn't resolve to a row. Gateway
    /// → 401 (their session may have been revoked).
    UnknownUser,
}

#[derive(Clone)]
pub struct RatingService {
    repo: Arc<dyn RatingRepo>,
    users: Arc<dyn UserRepo>,
}

impl std::fmt::Debug for RatingService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RatingService").finish_non_exhaustive()
    }
}

impl RatingService {
    #[must_use]
    pub fn new(repo: Arc<dyn RatingRepo>, users: Arc<dyn UserRepo>) -> Self {
        Self { repo, users }
    }

    /// Insert or update the caller's score for `(target_kind,
    /// target_id)`. The anti-spam gate runs first so a
    /// too-young account doesn't see a misleading "ok" then
    /// silently get re-checked at the next step.
    pub async fn upsert(
        &self,
        user_id: Uuid,
        target_kind: RatingTargetKind,
        target_id: Uuid,
        score: i16,
    ) -> Result<Result<RatingRow, RatingDenialReason>, AuthError> {
        if !(SCORE_MIN..=SCORE_MAX).contains(&score) {
            return Ok(Err(RatingDenialReason::ScoreOutOfRange));
        }
        match self.check_account_age(user_id, Utc::now()).await? {
            Ok(()) => {}
            Err(reason) => return Ok(Err(reason)),
        }
        let row = self
            .repo
            .upsert(user_id, target_kind, target_id, score, Utc::now())
            .await?;
        Ok(Ok(row))
    }

    /// Delete the caller's rating. Returns `Ok(false)` when
    /// the row was already absent (idempotent).
    pub async fn withdraw(
        &self,
        user_id: Uuid,
        target_kind: RatingTargetKind,
        target_id: Uuid,
    ) -> Result<bool, AuthError> {
        Ok(self
            .repo
            .withdraw(user_id, target_kind, target_id, Utc::now())
            .await?)
    }

    /// Aggregate + viewer's own score (if signed in). Works
    /// for anonymous callers: returns the aggregate with
    /// `viewer_score = None`.
    pub async fn view(
        &self,
        target_kind: RatingTargetKind,
        target_id: Uuid,
        viewer_id: Option<Uuid>,
    ) -> Result<RatingView, AuthError> {
        Ok(self.repo.view(target_kind, target_id, viewer_id).await?)
    }

    /// Verify the caller's account is older than
    /// [`MIN_ACCOUNT_AGE_FOR_RATING`]. `now` is taken from
    /// the caller so tests can pass a fixed clock.
    async fn check_account_age(
        &self,
        user_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<Result<(), RatingDenialReason>, AuthError> {
        let Some(user) = self.users.find_user_by_id(user_id).await? else {
            return Ok(Err(RatingDenialReason::UnknownUser));
        };
        if now - user.created_at < MIN_ACCOUNT_AGE_FOR_RATING {
            return Ok(Err(RatingDenialReason::AccountTooNew));
        }
        Ok(Ok(()))
    }
}
