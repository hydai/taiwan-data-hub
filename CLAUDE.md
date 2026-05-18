# CLAUDE.md — Project memory for Taiwan Data Hub

> Per-project guidance for Claude Code (and any agentic coding assistant)
> working in this repository. Loaded into every Claude session — keep
> short and high-signal.

## Project context

**Taiwan Data Hub** is a fully open-source, self-hostable MCP (Model Context
Protocol) service hub that aggregates Taiwan public data sources and exposes
them to AI agents like Claude Desktop, Cursor, and Cline.

- **Status**: pre-alpha · design phase (no implementation code yet)
- **License**: Apache-2.0
- **Canonical design**: [`docs/DESIGN.md`](docs/DESIGN.md) — read this first
- **Roadmap**: [Project #2](https://github.com/users/hydai/projects/2) · 80 sub-issues across 9 milestones
- **MVP (v0.1)** = M0 + M1 + M2 → 28 P0 issues

## Tech stack (versions pinned as of 2026-05-18)

| Layer | Stack |
|---|---|
| Backend | Rust · axum 0.8.8 · sqlx 0.8.6 · polars 0.53 · rmcp 1.7 |
| Frontend | SvelteKit 2.59 · Svelte 5.55 (Runes) · Tailwind 4.3 (CSS-first) · shadcn-svelte 1.2 |
| Data | PostgreSQL 18 (UUIDv7) · Parquet · SeaweedFS (S3-compat) · DragonflyDB · Meilisearch 1.15 |
| Spec | MCP 2025-11-25 · OAuth 2.1 + PKCE · OpenAPI 3.1 (utoipa 5.5) |
| Deploy | Docker Compose (profiles: default / full / obs / dev) · Caddy 2.11 |

Full version table and rationale: [`docs/DESIGN.md` §5](docs/DESIGN.md#5-關鍵函式庫選擇版本驗證至-2026-05-18).

## Critical gotchas (do NOT copy from old examples)

These libraries had breaking changes within the last 12 months. Always check
versions before copy-pasting from blog posts or AI training data:

1. **axum 0.8** — path syntax `/:id` panics at startup; use `/{id}`.
2. **Tailwind v4** — no `tailwind.config.js`; use `@theme` block in CSS, `@import "tailwindcss"`, plugin `@tailwindcss/postcss`. `shadow` renamed: `shadow-sm` → `shadow-xs`.
3. **Svelte 5 Runes** — use `$state` / `$derived` / `$effect` / `$props`; never `$:` or `export let`.
4. **rmcp 1.x** — not 0.x; trait signatures and imports changed completely.
5. **utoipa 5.x** — use `OpenApiRouter`, OpenAPI 3.1 schema paths.
6. **oauth2 / openidconnect 5.x** — async client now trait-based.
7. **thiserror 2.0** — most cases rebuild fine; check changelog for `#[backtrace]`.
8. **Paraglide-JS v2** — API renamed: `getLocale()` / `setLocale()` / `locales` (NOT `languageTag()` / `availableLanguageTags`); server middleware moved to `server.js` as `paraglideMiddleware`.
9. **MapLibre GL v5** — style spec & worker config changed.
10. **echarts v6** — v5 → v6 not fully compatible; do NOT use `svelte-echarts` (stale 1.0.0); write a 30-line wrapper.
11. **MinIO is archived (2026-04-25)** — use SeaweedFS or Garage instead.
12. **PostgreSQL 18** — has native `uuidv7()`; prefer over manual UUID generation for sortable ids.

## Repo layout

```
.
├── CLAUDE.md                      # this file
├── README.md
├── LICENSE                        # Apache-2.0
├── CONTRIBUTING.md                # human contributor guide
├── .gitmessage                    # commit template (auto-loaded via git config)
├── .githooks/commit-msg           # local commit format validator
├── .github/workflows/             # CI
├── docs/
│   └── DESIGN.md                  # canonical design (read first)
├── scripts/
│   ├── setup.sh                   # one-shot dev environment setup
│   ├── create-issues.sh           # batch-create GitHub issues (already run)
│   └── populate-project.py        # populate Projects v2 board (already run)
├── Cargo.toml                     # (to be added in M0 #0.1) — Rust workspace
├── crates/                        # (to be added in M0 #0.1)
│   ├── gateway/                   # Axum HTTP + MCP gateway
│   ├── mcp-stdio/                 # stdio shim for Claude Desktop
│   ├── etl-worker/                # cron-driven ingest
│   ├── mcp-core/                  # MCP dispatcher + protocol types
│   ├── tools-utility/             # 53 TW utility tools
│   ├── tools-data/                # MCP data tools (base + rich)
│   ├── connectors/                # SourceConnector trait + impls
│   ├── storage/                   # sqlx repos + Parquet IO
│   ├── auth/                      # password + OAuth + DCR
│   ├── shared/                    # error, telemetry, config, i18n
│   └── test-support/              # testcontainers helpers
├── web/                           # (to be added in M0 #0.2) — SvelteKit
├── migrations/                    # (to be added in M0 #0.8) — sqlx-cli
├── docker/                        # (to be added in M0 #0.3) — compose files
└── config/                        # sources.toml, tiers.toml, domains.yaml
```

## Commands

### One-time setup

```bash
./scripts/setup.sh              # installs git hooks, sets commit template
```

### Build / test (once crates exist)

```bash
# Rust (always release builds per project rule)
cargo build --release
cargo clippy --release -- -D warnings
cargo test --release
cargo fmt --check

# SvelteKit
pnpm --filter web dev
pnpm --filter web build
pnpm --filter web check
pnpm --filter web lint

# Docker
docker compose up -d                       # default profile
docker compose --profile full up -d        # adds dragonfly + meili + seaweedfs
docker compose --profile obs up -d         # adds otel-collector + prometheus

# DB migrations
sqlx migrate run                           # apply migrations
sqlx migrate add <name>                    # create new migration

# Issue / project automation (already executed once)
REPO=hydai/taiwan-data-hub bash scripts/create-issues.sh
python3 scripts/populate-project.py
```

### Pre-commit checks

```bash
lineguard <changed-files>                  # format / line-ending check
cargo clippy -- -D warnings                # before any Rust commit
pnpm prettier --check 'web/**/*'           # before any frontend commit
```

A PreToolUse hook in `~/.claude/settings.json` auto-runs these before `git commit`.

## Commit message format (REQUIRED)

This repository uses **Conventional Commits** with a small project-specific
extension. The commit-msg hook will reject non-conforming messages.

```
<type>(<scope>): <subject> (#<sub-issue-id>)

<body — wrap at 72 cols, explain WHY, not WHAT>

<footer>
```

### Allowed `<type>`

| Type | Use for |
|---|---|
| `feat` | new user-visible feature |
| `fix` | bug fix |
| `docs` | docs only (incl. comments) |
| `refactor` | code change neither feat nor fix |
| `chore` | tooling, build, deps, project meta |
| `test` | adding or fixing tests |
| `perf` | performance improvement |
| `build` | build system or external deps |
| `ci` | CI / GitHub Actions |
| `style` | formatting, missing semicolons, no logic change |
| `revert` | revert a previous commit |

### Allowed `<scope>` (matches Project Component field)

`mcp` · `backend` · `frontend` · `etl` · `infra` · `docs` · `i18n` · `community` · `security` · `deps`

Scope is optional but encouraged. Omit for cross-cutting commits (e.g.,
multi-component refactors).

### `<subject>` rules

- imperative mood ("add", not "added" or "adds")
- lowercase, no trailing period
- ≤ 72 characters including type/scope/issue-ref

### `(#<sub-issue-id>)` — sub-issue reference

Append the sub-issue id from `docs/DESIGN.md §9` (e.g. `#0.1`, `#3.4`, `#5a.2`).
This links commits back to the design, NOT to the GitHub issue number — those
are tracked via `Closes #N` in the footer.

### Footer keywords

- `Closes #N` — closes GitHub issue N on merge
- `Refs #N` — references without closing
- `BREAKING CHANGE: <description>` — paired with `!` after type/scope
- `Signed-off-by: Name <email>` — DCO sign-off (required, use `git commit -s`)
- `Co-Authored-By: …` — credit pair-programmers / AI assistants

### Examples

```
feat(mcp): implement list_domains tool (#1.3)

Returns 20 domains seeded from config/domains.yaml with i18n names and
dataset counts. Uses the mcp-core registry pattern that the next 4
tools will reuse.

Closes #3
Signed-off-by: hydai <z54981220@gmail.com>
```

```
fix(etl): handle ETag mismatch on data.gov.tw (#1.4)

Upstream returns weak ETags during cache busts which differ from our
stored strong-form. Strip leading W/ before comparison.

Closes #5
Signed-off-by: hydai <z54981220@gmail.com>
```

```
chore(deps): bump axum 0.8.7 → 0.8.8
```

```
refactor(backend)!: change query_rows return shape (#1.7)

Nest rows + columns under a result object so future versions can add
metadata (truncated, ms, etc.) without breaking schema.

BREAKING CHANGE: query_rows MCP response now returns
{rows: [], columns: [], truncated: bool} instead of a bare array.

Closes #8
Signed-off-by: hydai <z54981220@gmail.com>
```

## Branch naming

```
<type>/<short-description>          # for solo work
<type>/<issue-num>-<short-desc>     # when linking to a GitHub issue

# examples
feat/list-domains-tool
fix/57-etag-weak-comparison
chore/bump-axum-0.8.8
docs/refine-mcp-quickstart
```

## PR flow

1. Branch from `main`
2. Commit with conventional format (hook enforces locally)
3. Push and open PR — title must also follow conventional format (GHA enforces)
4. **Merge prerequisites (run in parallel, all must clear):**
   - **CI green**: fmt + clippy + test + svelte-check + prettier + (later) lighthouse — kicked off automatically on every push
   - **Copilot review converges** to *"generated no new comments"* (see next section) — needs manual assignment per round
   - **Maintainer review** approves (where applicable)
5. Squash-merge — PR title becomes the merged commit subject; explicit `--subject`/`--body` flags to `gh pr merge` keep the merged message clean (the per-iteration commits get rolled up)
6. Delete branch after merge

## Code review with GitHub Copilot

Every PR gets a first-pass review from GitHub Copilot as the automated reviewer that runs in parallel with CI and human review (see the PR flow above). Copilot is good at catching cross-file consistency drift (docs vs code, hook vs CI rules, version pins vs lockfiles) — exactly the noise we don't want surfacing in human review.

### Assigning Copilot

```bash
# Use the bot slug, NOT the display name "Copilot".
# (gh CLI lowercases --add-reviewer input before lookup, so "Copilot"
# becomes "copilot" which 404s.)
gh pr edit <PR#> --add-reviewer copilot-pull-request-reviewer
```

Copilot takes 2–4 minutes to post its review.

### Processing comments

For each inline comment, decide:

- **Reasonable** → fix it in code, push the fix
- **Wrong / not applicable** → reply explaining why, then resolve anyway

Either way: reply on the thread (so reviewers understand your reasoning) and resolve the thread. The `resolveReviewThread` mutation is only on GraphQL, not REST.

```bash
# Fetch the first 50 review threads (with their resolved state) and
# filter to unresolved with jq — the GraphQL API has no isResolved
# argument on `reviewThreads`, so filtering happens client-side.
# Bump `first` or paginate via the connection's `pageInfo` for PRs
# with > 50 threads (rare):
gh api graphql -f query='
query {
  repository(owner: "hydai", name: "taiwan-data-hub") {
    pullRequest(number: <PR#>) {
      reviewThreads(first: 50) {
        nodes {
          id  isResolved
          comments(first: 1) { nodes { databaseId path line body } }
        }
      }
    }
  }
}' | jq '.data.repository.pullRequest.reviewThreads.nodes[] | select(.isResolved | not)'

# Reply to a comment (REST, by databaseId):
gh api -X POST /repos/hydai/taiwan-data-hub/pulls/<PR#>/comments/<comment_id>/replies \
  -f 'body=Your reasoning here.'

# Resolve the thread (GraphQL, by thread node id):
gh api graphql -f query='
mutation { resolveReviewThread(input: {threadId: "<thread_id>"}) { thread { isResolved } } }'
```

### Triggering the next review round

Copilot does NOT auto-re-review on subsequent pushes. After pushing fixes, re-assign:

```bash
gh pr edit <PR#> --add-reviewer copilot-pull-request-reviewer
```

Then wait another 2–4 minutes. Copilot's review summary explicitly says either:

- *"Copilot reviewed N of M files and generated K comments"* — keep iterating
- *"Copilot reviewed N of M files and generated no new comments"* — loop terminates; proceed to squash-merge

### Squash-merge with curated message

```bash
gh pr merge <PR#> --squash --delete-branch \
  --subject "feat(scope): subject (#<sub-issue-id>)" \
  --body "$(cat <<'EOF'
…curated summary, references "Closes #<gh-issue>", trailers…
EOF
)"
```

The per-round commit messages (`fix: address Copilot 2nd-pass…`) disappear from `main`; only the curated squash message lands. Copilot iteration history remains visible in the PR conversation.

## Quality bars (PR-blocking)

- `cargo clippy --release -- -D warnings` — no warnings
- `cargo test --release` — all pass
- `cargo fmt --check` — formatted
- `pnpm --filter web check` — Svelte type-checks
- `pnpm prettier --check` — formatted
- Lighthouse perf ≥ 85, a11y ≥ 95, best-practices ≥ 90, SEO ≥ 90 (frontend PRs only)
- Test coverage for new logic (no hard threshold; reviewer judgement)

## Security must-knows

- Never log secrets — `tracing` events stripped via filter
- All DB OAuth tokens stored AES-GCM-encrypted with env-supplied KEK
- `query_rows` user SQL passes through sqlparser-rs AST whitelist
- All inbound API keys/passwords hashed (argon2id for passwords, sha256 for keys)
- Rate limit at 3 layers (IP / user / tool)
- Files uploads: MIME allowlist + `infer` magic-byte check + size cap

See `docs/DESIGN.md §6` for full security model.

## i18n

- **zh-TW is source language** for both UI strings and DB content
- Fallback chain: requested locale → zh-TW
- Use `getLocale()` / `setLocale()` (Paraglide v2 names; NOT v1's `languageTag()`)
- DB i18n columns: `jsonb` shape `{"zh-TW": "...", "en": "..."}`; read via
  `COALESCE(col->>$lang, col->>'zh-TW')`

## Operating modes

The gateway honors `MODE=personal|multi-user` (default `personal`):

- `personal` — no auth required, single user, suitable for laptop use. Login UI hidden.
- `multi-user` — full auth required for contributions and API keys; reads remain public.

CI must run e2e under both modes.

## When in doubt

1. Check `docs/DESIGN.md` — it has the canonical answer for architecture decisions
2. Check the relevant sub-issue's "Definition of Done" on GitHub
3. Open a Discussion on the repo before writing code that touches multiple components
4. Tag a maintainer in a PR draft early if uncertain
