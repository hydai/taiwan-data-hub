//! Comment service (#5a.3).
//!
//! Thin business-logic layer on top of
//! [`storage::CommentRepo`]. Responsibilities:
//!
//!   * Validation — body length, reply depth (max 1 nested),
//!     edit-window cutoff (5 minutes by default).
//!   * Markdown → sanitized HTML rendering via
//!     `comrak` + `ammonia`.
//!   * Soft-delete tombstone substitution on the read path.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use comrak::{ComrakOptions, markdown_to_html};
use storage::{CommentRepo, CommentRow, CommentTargetKind, NewComment};
use uuid::Uuid;

use crate::error::AuthError;

/// Default edit window — 5 minutes per the #5a.3 spec.
/// Edits past this point return
/// [`CommentDenialReason::EditWindowClosed`].
#[allow(clippy::doc_markdown)]
pub const DEFAULT_EDIT_WINDOW: Duration = Duration::from_secs(5 * 60);

/// Maximum Markdown body length (post-trim). 8 KiB is the
/// product cap; anything more belongs in an external doc the
/// commenter links to. Mirrors the limit on submission
/// descriptions but doubled — a discussion comment commonly
/// quotes more than a submission pitch.
pub const MAX_COMMENT_BODY_LEN: usize = 8192;

/// Rendered shape returned to API callers. The original
/// Markdown stays in `body_md` (null on a soft-delete); the
/// HTML in `body_html` is what the UI renders. A soft-deleted
/// row carries `body_md = None` and `body_html = "[deleted]"`.
#[derive(Debug, Clone)]
pub struct RenderedComment {
    pub id: Uuid,
    pub target_kind: CommentTargetKind,
    pub target_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub depth: i16,
    pub body_md: Option<String>,
    /// Pre-rendered HTML (sanitized via ammonia). The web
    /// layer drops it into the DOM with `{@html}` — the
    /// sanitiser is the load-bearing XSS guard.
    pub body_html: String,
    pub created_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
    /// `true` iff the row is soft-deleted. The web layer
    /// branches on this to hide the Edit / Delete / Reply
    /// affordances; `body_html` already carries the
    /// `[deleted]` tombstone content, so no separate
    /// styling decision is required.
    pub is_deleted: bool,
}

/// Why a comment write rejected. Distinct variants so the
/// gateway can pick the right HTTP status. The current
/// `gateway::comments_routes` mapping is `404`, `409`, and
/// `400` (the latter shared across `DepthCapExceeded`,
/// `ParentNotFound`, and `InvalidBody`); the `401` for the
/// "no session" case is owned by the route handler, not by
/// this enum. No `403` is emitted today because comment
/// writes don't need a role gate (any authenticated user can
/// post on their own).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentDenialReason {
    /// Author mismatch — caller is not the comment's owner.
    /// Folded with "not found" so an attacker can't probe.
    NotFoundOrNotYours,
    /// The edit window (default 5 min) has elapsed since the
    /// comment was first posted.
    EditWindowClosed,
    /// The author tried to reply at depth 2+; the schema
    /// caps threading at one nesting level.
    DepthCapExceeded,
    /// The parent id either doesn't exist OR is attached to a
    /// different `(target_kind, target_id)` than the new
    /// reply. The gateway maps this into 400 (bad reference).
    ParentNotFound,
    /// Body validation failed — empty (post-trim) or too long.
    InvalidBody(BodyError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyError {
    Empty,
    TooLong,
}

#[derive(Clone)]
pub struct CommentService {
    comments: Arc<dyn CommentRepo>,
    edit_window: Duration,
}

impl std::fmt::Debug for CommentService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommentService").finish_non_exhaustive()
    }
}

impl CommentService {
    #[must_use]
    pub fn new(comments: Arc<dyn CommentRepo>) -> Self {
        Self {
            comments,
            edit_window: DEFAULT_EDIT_WINDOW,
        }
    }

    /// Override the edit window. Used by tests; production
    /// uses [`DEFAULT_EDIT_WINDOW`].
    #[must_use]
    pub fn with_edit_window(mut self, window: Duration) -> Self {
        self.edit_window = window;
        self
    }

    /// Render every comment attached to a target. Returned
    /// rows are in `created_at ASC` order; the web layer
    /// builds the parent → reply tree by walking
    /// `parent_id`.
    ///
    /// Markdown rendering (comrak + ammonia) is CPU-bound, so
    /// the whole batch runs inside a single
    /// [`tokio::task::spawn_blocking`] to keep async worker
    /// threads free under load. The list endpoint is public
    /// and can be hit repeatedly; per-row inline rendering
    /// would stall the runtime on a hot thread.
    pub async fn list_for_target(
        &self,
        target_kind: CommentTargetKind,
        target_id: Uuid,
    ) -> Result<Vec<RenderedComment>, AuthError> {
        let rows = self
            .comments
            .list_for_target(target_kind, target_id)
            .await?;
        let rendered = tokio::task::spawn_blocking(move || {
            rows.into_iter().map(render_row).collect::<Vec<_>>()
        })
        .await
        .map_err(|e| {
            // `JoinError` only fires if the blocking task
            // panicked or the runtime is shutting down —
            // either is a programmer bug, not user input.
            AuthError::Internal(format!("comment-render task failed: {e}"))
        })?;
        Ok(rendered)
    }

    /// Create a new comment. `parent_id` is `Some` for a reply
    /// (depth 1) and `None` for a root (depth 0). The service
    /// validates the body + (for replies) the parent's depth
    /// and target before delegating to the repo.
    pub async fn create(
        &self,
        author_id: Uuid,
        target_kind: CommentTargetKind,
        target_id: Uuid,
        parent_id: Option<Uuid>,
        body_md: String,
    ) -> Result<Result<RenderedComment, CommentDenialReason>, AuthError> {
        let trimmed = body_md.trim().to_owned();
        if let Err(err) = validate_body(&trimmed) {
            return Ok(Err(CommentDenialReason::InvalidBody(err)));
        }

        let depth = if let Some(parent) = parent_id {
            let Some(parent_row) = self.comments.get(parent).await? else {
                return Ok(Err(CommentDenialReason::ParentNotFound));
            };
            if parent_row.target_kind != target_kind || parent_row.target_id != target_id {
                return Ok(Err(CommentDenialReason::ParentNotFound));
            }
            // Refuse replies on a soft-deleted parent. The
            // frontend already hides the Reply affordance on
            // tombstoned rows, but the server is the
            // authoritative gate — collapse to `ParentNotFound`
            // so the wire shape stays the same as "id doesn't
            // exist" (a tombstoned parent is, from the
            // commenter's perspective, the same outcome).
            if parent_row.deleted_at.is_some() {
                return Ok(Err(CommentDenialReason::ParentNotFound));
            }
            if parent_row.depth >= 1 {
                return Ok(Err(CommentDenialReason::DepthCapExceeded));
            }
            1
        } else {
            0
        };

        let now = Utc::now();
        let id = self
            .comments
            .insert(NewComment {
                target_kind,
                target_id,
                parent_id,
                user_id: author_id,
                body_md: trimmed.clone(),
                depth,
                created_at: now,
            })
            .await?;

        // Render synchronously from the canonical state so the
        // response carries the HTML the next list call would
        // emit — saves the client a follow-up GET.
        Ok(Ok(render_row(CommentRow {
            id,
            target_kind,
            target_id,
            parent_id,
            user_id: Some(author_id),
            body_md: Some(trimmed),
            depth,
            created_at: now,
            edited_at: None,
            deleted_at: None,
        })))
    }

    /// Author-side edit. Body validation + the edit-window
    /// guard both fire here; the DB-side guard in the repo's
    /// UPDATE predicate is the race-safe backstop.
    pub async fn edit(
        &self,
        author_id: Uuid,
        comment_id: Uuid,
        new_body: String,
    ) -> Result<Result<RenderedComment, CommentDenialReason>, AuthError> {
        let trimmed = new_body.trim().to_owned();
        if let Err(err) = validate_body(&trimmed) {
            return Ok(Err(CommentDenialReason::InvalidBody(err)));
        }
        let now = Utc::now();
        // Pre-fetch lets the service distinguish "edit window
        // closed" from "not found / not yours" so the HTTP
        // boundary maps each to its own status (409 vs 404)
        // — the repo's UPDATE predicate would otherwise fold
        // both into `Ok(None)`. The DB predicate stays as a
        // race-safe backstop in case the row's `created_at`
        // crosses the cutoff between this read and the
        // UPDATE.
        let Some(existing) = self.comments.get(comment_id).await? else {
            return Ok(Err(CommentDenialReason::NotFoundOrNotYours));
        };
        if existing.user_id != Some(author_id) || existing.deleted_at.is_some() {
            return Ok(Err(CommentDenialReason::NotFoundOrNotYours));
        }
        // Compare in milliseconds so sub-second windows
        // (used by tests) and second+ windows (production
        // default 5 min) both behave correctly. Truncating
        // to `num_seconds()` would round 1.2s → 1, letting a
        // 1-second window pass at +1.2s elapsed.
        //
        // `as_millis()` returns `u128`; cast to `i64` via
        // `try_into` is overkill for any realistic edit
        // window (5 min = 300_000 ms), but the `i64::MAX` ms
        // clamp keeps the boundary explicit if a caller ever
        // supplies an absurdly long window.
        let elapsed_ms = (now - existing.created_at).num_milliseconds();
        let window_ms = i64::try_from(self.edit_window.as_millis()).unwrap_or(i64::MAX);
        if elapsed_ms < 0 || elapsed_ms > window_ms {
            return Ok(Err(CommentDenialReason::EditWindowClosed));
        }
        // The DB-side guard uses integer seconds (its
        // `$edit_secs::bigint * INTERVAL '1 second'`
        // expression); round up so a sub-second configured
        // window doesn't collapse to "0 seconds" and let the
        // SQL predicate pass when the service would have
        // rejected. `i64::div_ceil` is unstable, so do the
        // manual `(a + b - 1) / b` form, with a saturating
        // add so an absurdly long window (clamped to
        // `i64::MAX` above) doesn't overflow.
        let edit_secs = window_ms.saturating_add(999) / 1000;
        let Some(row) = self
            .comments
            .edit(comment_id, author_id, &trimmed, edit_secs, now)
            .await?
        else {
            // The UPDATE returned no rows. Possible causes:
            //
            //   1. Edit window crossed the cutoff between the
            //      pre-read above and the UPDATE.
            //   2. Another moderator soft-deleted the comment
            //      in the same window.
            //   3. The author hard-revoked / abandoned the
            //      session and the row's `user_id` mismatched.
            //
            // Distinguish (1) from (2/3) by re-reading the row.
            // A still-present, still-owned, still-non-deleted
            // row → window closed. Anything else → 404. This
            // mirrors the spec the pre-read above started.
            let recheck = self.comments.get(comment_id).await?;
            let still_eligible = recheck.is_some_and(|r| {
                r.user_id == Some(author_id) && r.deleted_at.is_none()
            });
            return Ok(Err(if still_eligible {
                CommentDenialReason::EditWindowClosed
            } else {
                CommentDenialReason::NotFoundOrNotYours
            }));
        };
        Ok(Ok(render_row(row)))
    }

    /// Author-side soft-delete. Returns the post-delete row
    /// (tombstone-rendered) so the client can replace the
    /// inline UI without a follow-up GET.
    pub async fn delete(
        &self,
        author_id: Uuid,
        comment_id: Uuid,
    ) -> Result<Result<RenderedComment, CommentDenialReason>, AuthError> {
        let now = Utc::now();
        let Some(row) = self.comments.delete(comment_id, author_id, now).await? else {
            return Ok(Err(CommentDenialReason::NotFoundOrNotYours));
        };
        Ok(Ok(render_row(row)))
    }
}

fn validate_body(trimmed: &str) -> Result<(), BodyError> {
    if trimmed.is_empty() {
        return Err(BodyError::Empty);
    }
    if trimmed.chars().count() > MAX_COMMENT_BODY_LEN {
        return Err(BodyError::TooLong);
    }
    Ok(())
}

fn render_row(row: CommentRow) -> RenderedComment {
    let is_deleted = row.deleted_at.is_some();
    // Soft-deleted rows render a plain `[deleted]` paragraph;
    // the web layer keys styling off the `is_deleted` flag
    // rather than a backend-injected class name (Tailwind
    // can't see server-generated selectors).
    let body_html = if is_deleted {
        "<p>[deleted]</p>".to_owned()
    } else {
        render_markdown(row.body_md.as_deref().unwrap_or(""))
    };
    RenderedComment {
        id: row.id,
        target_kind: row.target_kind,
        target_id: row.target_id,
        parent_id: row.parent_id,
        user_id: row.user_id,
        depth: row.depth,
        body_md: row.body_md,
        body_html,
        created_at: row.created_at,
        edited_at: row.edited_at,
        deleted_at: row.deleted_at,
        is_deleted,
    }
}

/// Markdown → sanitized HTML pipeline.
///
/// 1. `comrak` runs `CommonMark` with `unsafe_` disabled, so
///    raw HTML in the source is escaped rather than passed
///    through.
/// 2. `ammonia` runs its conservative whitelist over the
///    output — even if comrak ever regresses, ammonia strips
///    everything outside the allowed tag/attr set.
///
/// Both layers together are the load-bearing XSS guard. The
/// web client renders the result via `{@html}`; the inline
/// CSP nonces on the rest of the page do not save us if
/// `body_html` ever contained a `<script>`.
fn render_markdown(md: &str) -> String {
    let mut opts = ComrakOptions::default();
    // Disable raw-HTML passthrough — comrak escapes any
    // tag-shaped tokens in the source. Even if a future
    // option flips, ammonia's whitelist filters the output.
    opts.render.unsafe_ = false;
    // Auto-link bare URLs is fine; ammonia will keep only
    // safe schemes.
    opts.extension.autolink = true;
    // Strikethrough is harmless and improves expressiveness.
    opts.extension.strikethrough = true;
    let html = markdown_to_html(md, &opts);
    ammonia::clean(&html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_body_rejects_empty_and_too_long() {
        assert_eq!(validate_body(""), Err(BodyError::Empty));
        let long: String = "x".repeat(MAX_COMMENT_BODY_LEN + 1);
        assert_eq!(validate_body(&long), Err(BodyError::TooLong));
        assert!(validate_body("hi").is_ok());
    }

    #[test]
    fn renders_basic_markdown() {
        let html = render_markdown("**bold** and `code`");
        assert!(html.contains("<strong>bold</strong>"));
        assert!(html.contains("<code>code</code>"));
    }

    #[test]
    fn strips_script_tags() {
        let html = render_markdown("<script>alert('xss')</script> hi");
        assert!(!html.contains("<script"));
        assert!(html.contains("hi"));
    }

    #[test]
    fn strips_onclick_attribute() {
        let html = render_markdown("[link](https://example.com){onclick=\"bad()\"}");
        assert!(!html.contains("onclick"));
    }

    #[test]
    fn refuses_javascript_url_scheme() {
        let html = render_markdown("[click](javascript:alert(1))");
        // ammonia drops the unsafe href, leaving the anchor
        // with no `href` (or stripping it entirely). The
        // critical assertion is that the URL doesn't survive.
        assert!(!html.contains("javascript:"));
    }
}
