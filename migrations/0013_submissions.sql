-- #5a.1 user submissions (dataset / tool / connector / playground).
--
-- Authenticated users contribute one of four kinds of community
-- entries through the SvelteKit `/submit` form. Every submission
-- lands here in `status='pending'` and stays in the moderation
-- queue (#5a.2) until a curator approves, rejects, or the
-- author withdraws it.
--
-- Design choices:
--
--   * `submission_kind` is plain TEXT + CHECK (not an ENUM) so
--     adding a future kind ("notebook", "agent recipe") is a
--     one-line ALTER instead of an `ALTER TYPE ... ADD VALUE`
--     round trip. The four MVP kinds are pinned in the CHECK so
--     a typo in INSERT is rejected at write time.
--   * `status` likewise; the four states the moderation queue
--     reads are pinned, and new states ("needs_revision",
--     "withdrawn") extend the CHECK without a column rewrite.
--   * `payload` is JSONB. The Rust submission service validates
--     the per-kind shape BEFORE writing — the column itself is
--     opaque to Postgres, so a backend schema bump never needs a
--     migration on the row store. Search indexing of payload
--     fields lands separately in M5b's provenance work; for now
--     the moderation UI displays `title` + `submission_kind` and
--     reads the typed `payload` server-side.
--   * `title` is materialised out of the payload at write time
--     so the moderation queue can list submissions without
--     parsing every JSONB. Author can never edit this directly
--     once written (the service derives it from the typed
--     payload field for each kind).
--   * `reviewed_at` / `reviewed_by` / `review_reason` are NULL
--     until a moderator decides. They are NOT split into a
--     separate `submission_reviews` table because a submission
--     has at most one terminal decision; if we later add a
--     revision history we move them out then.
--   * Foreign keys use `ON DELETE` policies that match the auth
--     model: a deleted user CASCADEs their authored submissions
--     (GDPR-style erasure); a deleted moderator SET NULLs the
--     `reviewed_by` so the audit trail keeps the timestamp +
--     reason even when the reviewer's account is gone.

CREATE TABLE submissions (
    id              UUID         PRIMARY KEY DEFAULT uuidv7(),
    user_id         UUID         NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- One of: dataset / tool / connector / playground. The Rust
    -- service maps this to a typed payload struct on read and
    -- validates the JSONB before writing.
    submission_kind TEXT         NOT NULL,
    -- One of: pending / approved / rejected / withdrawn.
    -- `pending` is the initial state set by the create handler;
    -- `approved` / `rejected` are set by moderators (#5a.2);
    -- `withdrawn` is set by the author themselves.
    status          TEXT         NOT NULL DEFAULT 'pending',
    -- Short summary derived from the payload at write time.
    -- Surfaced in the moderation queue list view so curators
    -- don't have to open every row to triage. Free text from the
    -- author, capped at 200 chars in the service layer.
    title           TEXT         NOT NULL,
    -- Per-kind typed payload. The Rust submission service is
    -- the only writer; it serialises a tagged enum so the JSONB
    -- always carries a `{"kind": "...", ...}` discriminator at
    -- the top level matching the column above.
    payload         JSONB        NOT NULL,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    -- Moderator decision metadata. All three columns flip
    -- atomically in a single UPDATE when a curator
    -- approves / rejects.
    reviewed_at     TIMESTAMPTZ,
    reviewed_by     UUID         REFERENCES users(id) ON DELETE SET NULL,
    -- Free-form reason the moderator left for the author.
    -- Required on `rejected`, optional on `approved`. The
    -- service enforces the presence; the column itself stays
    -- nullable so an `approved` row without a note is
    -- representable.
    review_reason   TEXT,
    CONSTRAINT submissions_kind_known CHECK (
        submission_kind IN ('dataset', 'tool', 'connector', 'playground')
    ),
    CONSTRAINT submissions_status_known CHECK (
        status IN ('pending', 'approved', 'rejected', 'withdrawn')
    ),
    -- A row that carries reviewer metadata MUST be in a
    -- terminal-with-review state. Conversely a pending /
    -- withdrawn row must NOT carry reviewer metadata.
    --
    -- `reviewed_by` is NOT required to be NOT NULL on
    -- terminal rows because the FK uses `ON DELETE SET NULL`:
    -- when a moderator's account is later deleted, Postgres
    -- nulls the reference but the timestamp + reason still
    -- carry the decision audit trail. Requiring `reviewed_by
    -- IS NOT NULL` here would block the FK's set-null action.
    -- `reviewed_at` IS required because a terminal status
    -- without a decision timestamp is a contradiction —
    -- there's no way to forge a null `reviewed_at` from a
    -- legitimate write path.
    CONSTRAINT submissions_review_consistency CHECK (
        (status IN ('approved', 'rejected')
            AND reviewed_at IS NOT NULL)
        OR (status IN ('pending', 'withdrawn')
            AND reviewed_at IS NULL AND reviewed_by IS NULL
            AND review_reason IS NULL)
    )
);

-- Author lookup ("my submissions" page).
CREATE INDEX submissions_user_idx
    ON submissions (user_id, created_at DESC);

-- Moderation queue lookup (pending first, oldest first so we
-- triage FIFO). The partial WHERE keeps the index small —
-- approved / rejected / withdrawn rows are never read through
-- this access path.
CREATE INDEX submissions_pending_idx
    ON submissions (created_at)
    WHERE status = 'pending';

-- Per-kind triage filter (moderator might want "show me only
-- the dataset submissions"). Composite so the query planner
-- can use it for `WHERE submission_kind=$1 ORDER BY created_at`.
CREATE INDEX submissions_kind_status_idx
    ON submissions (submission_kind, status, created_at DESC);

-- `updated_at` management lives in the SQL UPDATE statements,
-- not a trigger. The `users_set_updated_at()` trigger function
-- (defined in 0008) unconditionally overwrites `NEW.updated_at`
-- with `now()`, which would silently kill the
-- `GREATEST(updated_at, $now)` monotonic clamping the
-- submission repo uses. The pattern matches `mcp_api_keys`
-- (migration 0011) and `sessions` (0010), both of which write
-- `last_used_at` / `expires_at` directly with `GREATEST` clamps
-- and intentionally skip the trigger for the same reason. A row
-- written without an explicit `updated_at` still gets the
-- column's `DEFAULT now()` on INSERT, so no path leaves the
-- column NULL.
