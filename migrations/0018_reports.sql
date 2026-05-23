-- #5a.6 content reports / flag — moderator queue +
-- auto-hide threshold on community-facing rows.
--
-- One row per `(reporter, target_kind, target_id)` so a
-- user can't pile on the same target. Moderators read the
-- open queue and resolve each report with an action
-- (`hide`, `keep`, `delete`, `warn_author`). When a target
-- accumulates `REPORT_AUTO_HIDE_THRESHOLD` distinct
-- reporters, the service flips a `hidden_at` flag on the
-- backing row so the frontend can render a placeholder
-- without exposing the underlying body. The threshold
-- lives at the service layer (auth::reports), not in SQL,
-- so it can move without a schema migration.

CREATE TABLE reports (
    id                  UUID         PRIMARY KEY DEFAULT uuidv7(),
    -- Nullable so a user deletion doesn't cascade-delete
    -- the report; moderators still need to see what was
    -- flagged. The reporter's user_id is set NULL on
    -- account deletion via the FK below.
    reporter_id         UUID         REFERENCES users(id) ON DELETE SET NULL,
    target_kind         TEXT         NOT NULL,
    target_id           UUID         NOT NULL,
    -- Coarse-grained category the UI surfaces in a radio
    -- group. The service validates against the same set;
    -- extending it is a service + UI + CHECK update.
    reason_category     TEXT         NOT NULL,
    -- Optional free-form context the reporter can include
    -- ("this comment quotes a private DM", etc.).
    body                TEXT,
    created_at          TIMESTAMPTZ  NOT NULL DEFAULT now(),
    -- Set when a moderator dispositioned the report.
    resolved_at         TIMESTAMPTZ,
    resolved_by         UUID         REFERENCES users(id) ON DELETE SET NULL,
    -- The action the moderator took. The UI maps each
    -- to a sentence the reporter sees on their
    -- "Reports I filed" panel.
    action_taken        TEXT,
    -- Optional moderator note, surfaced back to the
    -- reporter only when explicitly opted in by the
    -- moderator (action_taken `warn_author` etc.).
    resolution_note     TEXT,
    CONSTRAINT reports_target_kind_known CHECK (
        target_kind IN ('comment', 'submission')
    ),
    CONSTRAINT reports_reason_category_known CHECK (
        reason_category IN ('spam', 'harassment', 'off_topic', 'illegal', 'inaccurate', 'other')
    ),
    CONSTRAINT reports_action_taken_known CHECK (
        action_taken IS NULL
        OR action_taken IN ('hide', 'keep', 'delete', 'warn_author')
    ),
    CONSTRAINT reports_resolved_atoms CHECK (
        -- Unresolved: all dispositioning columns must be
        -- NULL — including resolution_note, so a
        -- partially-written row can't slip through.
        (
            resolved_at IS NULL
            AND resolved_by IS NULL
            AND action_taken IS NULL
            AND resolution_note IS NULL
        )
        -- Resolved: action + timestamp required. Note is
        -- still optional — the moderator can disposition
        -- without leaving a comment.
        OR (resolved_at IS NOT NULL AND action_taken IS NOT NULL)
    ),
    -- One report per (reporter, target). Re-filing is a
    -- no-op via ON CONFLICT — the moderator only sees one
    -- entry per voice per target.
    CONSTRAINT reports_unique_per_reporter_target
        UNIQUE (reporter_id, target_kind, target_id)
);

-- Moderator queue lookup — "show me open reports oldest
-- first" so triage stays first-in-first-out.
CREATE INDEX reports_open_created_idx
    ON reports (created_at)
    WHERE resolved_at IS NULL;

-- Per-reporter listing for the account-page "Reports I
-- filed" panel.
CREATE INDEX reports_reporter_created_idx
    ON reports (reporter_id, created_at DESC);

-- Aggregation path — "how many UNRESOLVED reporters has
-- this (kind, id) accumulated?" — drives the auto-hide
-- threshold check on every insert. Partial predicate
-- matches the query exactly, so the per-target COUNT
-- stays index-only as resolved reports accumulate over
-- the lifetime of a target.
CREATE INDEX reports_unresolved_target_idx
    ON reports (target_kind, target_id)
    WHERE resolved_at IS NULL;

COMMENT ON TABLE reports IS
    'Per-(reporter, target) flag rows. UNIQUE forbids piling on; auto-hide threshold enforced by auth::ReportService.';

-- Auto-hide column on comments. Independent from
-- `deleted_at` so we can tell user-deletion apart from
-- moderator-driven (or threshold-driven) hide in audits.
ALTER TABLE comments
    ADD COLUMN hidden_at TIMESTAMPTZ;

COMMENT ON COLUMN comments.hidden_at IS
    'When non-NULL, the comment is hidden by community reports or a moderator. Body is still kept for audit; the renderer substitutes a placeholder.';

-- Submissions get a `hidden_at` column too; the
-- submission detail view (frontend) substitutes a
-- placeholder. Re-hiding via the existing `status` field
-- (e.g. moving to a new `hidden` value) would require
-- updating the M5a.2 moderation queue logic to skip the
-- new value everywhere — a separate column keeps the
-- moderation lifecycle simple.
ALTER TABLE submissions
    ADD COLUMN hidden_at TIMESTAMPTZ;

COMMENT ON COLUMN submissions.hidden_at IS
    'When non-NULL, the submission is hidden by community reports or a moderator. The submission lifecycle (pending/approved/rejected) is unaffected — hide is a parallel flag.';
