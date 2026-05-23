-- #5a.3 threaded comments on datasets (also tools / connectors).
--
-- A single `comments` table backs every commentable surface
-- via `(target_kind, target_id)` — matching the polymorphic
-- shape `audit_logs` already uses. Threading is capped at
-- depth 2 (one root + one reply level) by the
-- `comments_depth_max_two` CHECK, enforced from `depth`
-- rather than recursive walks so insert validation is O(1).
--
-- Design choices:
--
--   * `body_md` carries the author-supplied Markdown verbatim.
--     The Rust comment service renders it to HTML on read with
--     `comrak` + `ammonia` (sanitization) so the column stays
--     small and an XSS fix is a code deploy rather than a
--     migration over every historical row.
--   * `edited_at` is NULL until the author edits within the
--     5-minute edit window the service enforces. The column
--     itself stays nullable indefinitely so a soft-deleted
--     row can preserve its prior edit history flag.
--   * `deleted_at` + `body_md = NULL` on soft-delete preserves
--     the row (and its `id` for thread continuity) while
--     dropping the user-supplied bytes. The service renders
--     a tombstone ("[deleted]") to readers; replies stay
--     visible because the thread structure survives.
--   * `parent_id` self-references with `ON DELETE RESTRICT`:
--     a hard delete on a parent would orphan its replies, so
--     we keep the row alive (soft-delete is the only path).
--   * `target_kind` is plain TEXT + CHECK so future targets
--     (e.g. `playground`) extend with a one-line ALTER.
--   * `user_id` uses `ON DELETE SET NULL` — a deleted account
--     leaves an anonymous-tombstone comment in place rather
--     than vacuuming the thread.

CREATE TABLE comments (
    id              UUID         PRIMARY KEY DEFAULT uuidv7(),
    target_kind     TEXT         NOT NULL,
    target_id       UUID         NOT NULL,
    parent_id       UUID         REFERENCES comments(id) ON DELETE RESTRICT,
    user_id         UUID         REFERENCES users(id) ON DELETE SET NULL,
    -- Markdown source the author submitted. NULL after a
    -- soft-delete; the service then renders a tombstone.
    body_md         TEXT,
    -- Thread depth: 0 = root, 1 = reply. The CHECK below
    -- caps the column at those two values; un-capping past 1
    -- would need a CHECK update + a UI capable of rendering
    -- deeper nesting, so it's not just a flag flip today.
    depth           SMALLINT     NOT NULL,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    edited_at       TIMESTAMPTZ,
    deleted_at      TIMESTAMPTZ,
    CONSTRAINT comments_target_kind_known CHECK (
        target_kind IN ('dataset', 'tool', 'connector', 'playground')
    ),
    -- Cap reply nesting on this column. The DB only
    -- enforces `depth IN (0, 1)`; the "no reply to a depth-1
    -- row" rule is enforced at the service layer
    -- (`auth::CommentService::create`), which refuses to
    -- insert a `parent_id` whose row already has `depth = 1`.
    -- A row trigger could mirror that into the DB, but the
    -- service is the only writer in production so the CHECK
    -- here is intentionally just the column-level guard.
    CONSTRAINT comments_depth_max_two CHECK (depth IN (0, 1)),
    -- Root comments have no parent; reply comments must.
    CONSTRAINT comments_root_has_no_parent CHECK (
        (depth = 0 AND parent_id IS NULL)
        OR (depth = 1 AND parent_id IS NOT NULL)
    ),
    -- Soft-delete invariant: when `deleted_at` is set, the
    -- author-supplied bytes must be NULL — and vice versa.
    -- Keeps the column in lockstep with the lifecycle flag.
    CONSTRAINT comments_body_matches_deleted CHECK (
        (deleted_at IS NULL AND body_md IS NOT NULL)
        OR (deleted_at IS NOT NULL AND body_md IS NULL)
    )
);

-- Thread fetch by `(target_kind, target_id)` ordered by
-- `created_at` is the hot path the SvelteKit detail pages
-- run on every load. The composite index serves
-- `WHERE target_kind=$1 AND target_id=$2 ORDER BY
-- created_at ASC` without an extra sort.
CREATE INDEX comments_target_idx
    ON comments (target_kind, target_id, created_at);

-- Per-author lookup ("my comments" follow-up + moderator
-- audit). Partial filter on non-deleted, non-anonymised rows
-- keeps the index small for the "active comments by user X"
-- access pattern — `user_id` can become NULL via the FK's
-- `ON DELETE SET NULL`, and those rows are never looked up
-- by author.
CREATE INDEX comments_user_idx
    ON comments (user_id, created_at DESC)
    WHERE deleted_at IS NULL AND user_id IS NOT NULL;

COMMENT ON TABLE comments IS
    'Threaded comments (depth ≤ 1) on datasets / tools / connectors / playgrounds. body_md is the raw Markdown; the gateway renders it through comrak + ammonia on read.';
COMMENT ON COLUMN comments.depth IS
    '0 = root, 1 = reply. Cap enforced both at the service layer and via the comments_depth_max_two CHECK.';
