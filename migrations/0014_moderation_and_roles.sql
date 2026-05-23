-- #5a.2 moderation queue + role-based access.
--
-- Two additions to support the moderation workflow:
--
--   1. `users.role` — discriminates regular users from
--      moderator / curator / admin. Plain TEXT + CHECK (not an
--      ENUM) so adding a future role is a one-line ALTER
--      instead of an `ALTER TYPE ... ADD VALUE`. The role
--      lookup the moderation gate fires is served by the
--      existing PRIMARY KEY index on `users.id`; no extra
--      index needed.
--   2. `audit_logs` — append-only record of every moderator
--      decision, including approve/reject/promote, with the
--      acting user, the target row, and a free-form payload.
--      Driven by the spec line in DESIGN.md §6: "admin actions
--      + submission decision 全記 audit_logs 表, append-only".
--
-- The moderator-side transition columns on `submissions` (and
-- the `users` FK on `submissions.reviewed_by`) already landed
-- in 0013; this migration only adds the role + audit columns
-- the dispatcher needs.

ALTER TABLE users
    ADD COLUMN role TEXT NOT NULL DEFAULT 'user'
        CHECK (role IN ('user', 'moderator', 'curator', 'admin'));

-- The role lookup on the moderation gate is `SELECT role
-- FROM users WHERE id = $1` — that query is already covered
-- by the `users` table's PRIMARY KEY index, which is a
-- single btree probe per request. No extra index needed
-- here; a partial index on `role <> 'user'` would only help
-- if the lookup included the predicate, and the gate's call
-- shape doesn't (it needs the actual role value back so the
-- service can decide the deny reason).

COMMENT ON COLUMN users.role IS
    'Authorization tier: user (default) < moderator < curator < admin. Moderator+ can act on /api/v1/admin/* endpoints.';

-- Audit log — append-only by application convention. The
-- schema does NOT REVOKE UPDATE/DELETE here because the role
-- the gateway connects as is operator-controlled (see
-- `DATABASE_URL`); enforcing read-only-after-insert is a
-- deploy-time GRANT on the operator's chosen role rather
-- than something the migration knows. The application layer
-- never issues UPDATE/DELETE against this table, and a
-- planned post-MVP follow-up is to add a `REVOKE UPDATE,
-- DELETE ON audit_logs FROM <app_role>` migration once the
-- canonical role name is pinned.
--
-- Captures every decision so a future "who approved this
-- dataset" query can reconstruct the full history.
--
-- Shape choices:
--
--   * `actor_id` is nullable because a moderator account
--     deletion shouldn't cascade-delete the audit row that
--     was already in place — the timeline survives the
--     account. `ON DELETE SET NULL` mirrors the FK pattern
--     submissions.reviewed_by uses.
--   * `action` is plain TEXT with a CHECK listing the known
--     verbs. Future actions extend the CHECK without a column
--     rewrite (matches the rest of the migrations' style).
--   * `target_kind` + `target_id` are stored verbatim — no FK
--     so deletion of the target row (e.g. a dataset later
--     redacted) doesn't drop the audit log entry, and so
--     entries can refer to a row in any table.
--   * `metadata` is JSONB for variant per-action context
--     (e.g. the moderator's reason on a reject, or the
--     resulting dataset_id on an approve).
CREATE TABLE audit_logs (
    id            UUID         PRIMARY KEY DEFAULT uuidv7(),
    actor_id      UUID         REFERENCES users(id) ON DELETE SET NULL,
    action        TEXT         NOT NULL,
    target_kind   TEXT         NOT NULL,
    target_id     UUID,
    metadata      JSONB        NOT NULL DEFAULT '{}'::jsonb,
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT audit_logs_action_known CHECK (
        action IN (
            'submission.approve',
            'submission.reject',
            'submission.promote_dataset'
        )
    )
);

-- Lookup by target — answers "show me everything that
-- happened to this submission / dataset id".
CREATE INDEX audit_logs_target_idx
    ON audit_logs (target_kind, target_id, created_at DESC);

-- Lookup by actor — answers "show me everything moderator X
-- has decided this month".
CREATE INDEX audit_logs_actor_idx
    ON audit_logs (actor_id, created_at DESC)
    WHERE actor_id IS NOT NULL;

COMMENT ON TABLE audit_logs IS
    'Append-only moderator decision log. INSERT-only by application convention; a later migration will REVOKE UPDATE/DELETE once the canonical app role is pinned.';
