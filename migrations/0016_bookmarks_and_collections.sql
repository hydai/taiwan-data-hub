-- #5a.4 bookmarks (favorites) + user-defined collections.
--
-- Two tables for a "save for later" surface:
--
--   1. `bookmarks` — the heart-button state. Per-user UNIQUE
--      on `(user_id, target_kind, target_id)` so the
--      idempotent toggle endpoint can rely on a single INSERT
--      ... ON CONFLICT path.
--   2. `collections` + `collection_items` — user-defined
--      private folders. Each row in `collection_items` is a
--      `(target_kind, target_id)` reference, mirroring the
--      polymorphic shape `comments` + `audit_logs` already
--      use. Public-share follows in a later milestone.
--
-- Bookmarks and collections are decoupled on purpose: a user
-- can bookmark something without sorting it, and an item can
-- live in a collection without being "favourited". A future
-- "my hearts" → "starter collection" auto-migration is a
-- one-shot worker, not a schema change.

CREATE TABLE bookmarks (
    id              UUID         PRIMARY KEY DEFAULT uuidv7(),
    user_id         UUID         NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    target_kind     TEXT         NOT NULL,
    target_id       UUID         NOT NULL,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT bookmarks_target_kind_known CHECK (
        target_kind IN ('dataset', 'tool', 'connector', 'playground')
    ),
    -- One heart per user per target. Re-clicking the heart
    -- removes the row; clicking again creates a fresh row
    -- with a new `created_at`. UNIQUE drives the
    -- `ON CONFLICT (...) DO NOTHING` happy path in the
    -- toggle endpoint.
    CONSTRAINT bookmarks_unique_per_user_target UNIQUE (user_id, target_kind, target_id)
);

-- "My bookmarks" lookup — list every save the caller has
-- made, newest first. The composite covers the
-- `WHERE user_id = $1 ORDER BY created_at DESC` access
-- pattern without an extra sort.
CREATE INDEX bookmarks_user_created_idx
    ON bookmarks (user_id, created_at DESC);

-- "My bookmarks of kind X" — the kind-filtered listing path
-- the gateway also hits (the dataset page's pre-paint probe
-- uses `?kind=dataset`, and the /account/bookmarks tabs
-- filter the same way). The three-column composite ordered
-- by `created_at DESC` lets Postgres satisfy
-- `WHERE user_id = $1 AND target_kind = $2 ORDER BY
-- created_at DESC` with an index-only scan as bookmark
-- counts grow.
CREATE INDEX bookmarks_user_kind_created_idx
    ON bookmarks (user_id, target_kind, created_at DESC);

-- Reverse lookup — "is this row bookmarked by me?" answers
-- come from the UNIQUE index above; no separate index
-- needed.

COMMENT ON TABLE bookmarks IS
    'Per-user heart/favorite state on community-facing rows. UNIQUE per (user, target_kind, target_id).';

-- User-defined collections (private). Each collection is
-- a named bag of `(target_kind, target_id)` references.
CREATE TABLE collections (
    id              UUID         PRIMARY KEY DEFAULT uuidv7(),
    user_id         UUID         NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- User-supplied display name. UNIQUE per user so the
    -- "my collections" sidebar can't render duplicates;
    -- length cap lives at the service layer.
    name            TEXT         NOT NULL,
    description     TEXT,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT collections_unique_name_per_user UNIQUE (user_id, name)
);

CREATE INDEX collections_user_idx
    ON collections (user_id, created_at DESC);

CREATE TABLE collection_items (
    collection_id   UUID         NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    target_kind     TEXT         NOT NULL,
    target_id       UUID         NOT NULL,
    added_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),
    -- Composite PK: a target may live in many collections,
    -- but at most once per collection. CASCADE on the parent
    -- so deleting a collection takes its items with it.
    PRIMARY KEY (collection_id, target_kind, target_id),
    CONSTRAINT collection_items_target_kind_known CHECK (
        target_kind IN ('dataset', 'tool', 'connector', 'playground')
    )
);

-- "Which collections is this row in?" lookup. The PK above
-- already covers the (collection_id, …) direction; this
-- partial covers the reverse for the "starred datasets in
-- which folders?" detail page.
CREATE INDEX collection_items_target_idx
    ON collection_items (target_kind, target_id);

-- "List items in this collection, newest-first" — the
-- detail-page access pattern. The PK on
-- `(collection_id, target_kind, target_id)` filters but
-- doesn't order by `added_at`; this composite ordered
-- index covers `WHERE collection_id = $1 ORDER BY added_at
-- DESC` without a sort step.
CREATE INDEX collection_items_added_idx
    ON collection_items (collection_id, added_at DESC);

COMMENT ON TABLE collections IS
    'User-defined private collections (folders) over community-facing rows. Public-share lands in a follow-up milestone.';
