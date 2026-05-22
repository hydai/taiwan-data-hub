-- #4.2 email + password authentication.
--
-- Stores users + the single-use tokens that back email verification
-- and password-reset magic links. OAuth (#4.3 GitHub / #4.4 Google)
-- and sessions (#4.5) extend the schema in later migrations; the
-- columns this migration adds stay stable across those changes.

-- CITEXT folds case for the email lookup so the UNIQUE constraint
-- catches `Alice@example.com` vs `alice@example.com` collisions at
-- INSERT time instead of letting two accounts share a recovery
-- mailbox. Postgres 18 ships this in contrib but the extension
-- still needs to be loaded once.
CREATE EXTENSION IF NOT EXISTS citext;

CREATE TABLE users (
    id                UUID         PRIMARY KEY DEFAULT uuidv7(),
    email             CITEXT       NOT NULL UNIQUE,
    -- argon2id encoded hash (PHC string format: `$argon2id$v=19$m=...`).
    -- Holds salt + parameters, so we don't need a separate column.
    password_hash     TEXT         NOT NULL,
    -- NULL until the user clicks the verification magic link.
    -- Tools that gate on a verified mailbox compare IS NOT NULL.
    email_verified_at TIMESTAMPTZ,
    created_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ  NOT NULL DEFAULT now()
);

-- Single-use tokens backing email-verification and password-reset
-- links. We store ONLY a SHA-256 of the token so a DB leak doesn't
-- yield working magic links; the cleartext token only ever lives
-- in the recipient's mailbox.
--
-- `kind` is a plain TEXT column (not a Postgres ENUM) so the value
-- set can be widened without an `ALTER TYPE ... ADD VALUE` round
-- trip — but it carries a CHECK constraint listing the known kinds
-- so a typo in INSERT is rejected at write time. Introducing a new
-- flow therefore needs a one-line migration extending this CHECK;
-- existing rows aren't rewritten.
--
-- The partial index alongside is the "do they already have a
-- pending verification?" lookup. It is intentionally NOT UNIQUE:
-- a user clicking "resend" should mint a new token without
-- consuming the old, so two pending rows can legitimately co-exist
-- briefly. Both are valid until `consume_auth_token` redeems one.
CREATE TABLE auth_tokens (
    id          UUID         PRIMARY KEY DEFAULT uuidv7(),
    user_id     UUID         NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind        TEXT         NOT NULL,
    token_hash  BYTEA        NOT NULL UNIQUE,
    expires_at  TIMESTAMPTZ  NOT NULL,
    consumed_at TIMESTAMPTZ,
    created_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT auth_tokens_kind_known CHECK (kind IN ('email_verify', 'password_reset'))
);

CREATE INDEX auth_tokens_user_kind_idx
    ON auth_tokens (user_id, kind)
    WHERE consumed_at IS NULL;

-- Cleanup index for the v0.2 background job that periodically
-- prunes expired-but-unconsumed tokens. The partial filter keeps
-- the index small (already-consumed rows are excluded by
-- `auth_tokens_user_kind_idx`); the leading `expires_at` lets the
-- cleanup query do a range scan:
--
--   DELETE FROM auth_tokens
--    WHERE consumed_at IS NULL AND expires_at < now();
--
-- v0.1 does NOT yet run that cleanup — registration volume is low
-- enough that the unbounded growth is hours/days away from being
-- a problem on the target hardware. The cleanup job lands with
-- the etl-worker cron extension in v0.2; this index ships now so
-- the migration boundary doesn't change later.
CREATE INDEX auth_tokens_expires_idx
    ON auth_tokens (expires_at)
    WHERE consumed_at IS NULL;

-- Touch `updated_at` on every UPDATE so the rest of the codebase
-- can rely on it (e.g. "most recent password change wins" later).
CREATE OR REPLACE FUNCTION users_set_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at := now();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER users_set_updated_at_trg
    BEFORE UPDATE ON users
    FOR EACH ROW
    EXECUTE FUNCTION users_set_updated_at();
