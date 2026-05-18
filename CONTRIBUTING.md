# Contributing to Taiwan Data Hub

Thank you for your interest! This guide is for human contributors. If you are
an AI coding assistant (Claude, etc.), read [`CLAUDE.md`](CLAUDE.md) first.

## TL;DR

```bash
git clone https://github.com/hydai/taiwan-data-hub.git
cd taiwan-data-hub
./scripts/setup.sh                  # one-shot setup
# … hack …
git commit -s                       # uses the template; -s adds DCO sign-off
git push
gh pr create
```

The commit-msg hook will reject non-conforming messages. The GitHub Action
will reject non-conforming PR titles. Both rules are documented below.

## Project status

Pre-alpha — design phase. No production code yet. See [`docs/DESIGN.md`](docs/DESIGN.md)
and the [roadmap project](https://github.com/users/hydai/projects/2).

If you'd like to take an issue, comment on it first so we can avoid
duplicate work.

## Code of conduct

This project follows the [Contributor Covenant 2.1](https://www.contributor-covenant.org/version/2/1/code_of_conduct/).
Be respectful. Discrimination, harassment, and bad-faith arguments are not
welcome.

## Developer Certificate of Origin (DCO)

All commits must be signed off per the [Developer Certificate of Origin 1.1](https://developercertificate.org/).
By adding `Signed-off-by: Your Name <your.email@example.com>` to your commit,
you certify that you wrote the code (or have the right to submit it) under
this project's license (Apache-2.0).

The simplest way is to always commit with `-s`:

```bash
git commit -s -m "…"
# or, after running scripts/setup.sh, just:
git commit -m "…"     # -s implied via format.signOff=true
```

PRs without DCO sign-off will be blocked by CI.

## Commit message format (REQUIRED)

This project uses **Conventional Commits** with a project-specific extension
for the sub-issue id. The commit-msg hook enforces this locally.

### Shape

```
<type>(<scope>): <subject> (#<sub-issue-id>)

<body — wrap at 72, explain WHY>

<footer — Closes #N, BREAKING CHANGE:, Signed-off-by:>
```

### `<type>` (required) — one of

`feat` · `fix` · `docs` · `refactor` · `chore` · `test` · `perf` · `build` · `ci` · `style` · `revert`

### `<scope>` (optional) — one of

`mcp` · `backend` · `frontend` · `etl` · `infra` · `docs` · `i18n` · `community` · `security` · `deps`

These match the **Component** field on the project board so commit log and
board use the same vocabulary.

### `<subject>` rules

- imperative mood ("add", not "added")
- lowercase initial, no trailing period
- ≤ 72 characters including everything before the body

### `(#<sub-issue-id>)`

Reference the sub-issue id from `docs/DESIGN.md §9` (e.g. `#0.1`, `#3.4`,
`#5a.2`). This is the **design id**, not the GitHub issue number. Use the
footer `Closes #N` to link the GitHub issue.

For commits that touch multiple sub-issues, list the primary one in the
subject and the others in the body.

For trivial commits with no design id (typo fixes, dependency bumps), omit
the parenthetical.

### Breaking changes

Append `!` to the type/scope and add a `BREAKING CHANGE: <description>`
footer:

```
refactor(backend)!: change query_rows return shape (#1.7)

Nest rows + columns under a result object …

BREAKING CHANGE: query_rows MCP response now returns
{rows: [], columns: [], truncated: bool} instead of a bare array.

Closes #8
Signed-off-by: hydai <z54981220@gmail.com>
```

### More examples

```
feat(mcp): implement list_domains tool (#1.3)
fix(etl): handle ETag mismatch on data.gov.tw (#1.4)
docs: clarify SeaweedFS migration steps
chore(deps): bump axum 0.8.7 → 0.8.8
test(backend): cover sqlparser AST whitelist edge cases (#1.7)
ci: add lighthouse budget to frontend PRs (#2.10)
```

## Branch naming

```
<type>/<short-description>           # solo work
<type>/<gh-issue>-<short-desc>       # linked to GitHub issue

# good
feat/list-domains-tool
fix/57-etag-weak-comparison
chore/bump-axum-0.8.8

# bad
my-branch
hydai-fix
patch-1
```

## Pull request flow

1. **Branch** from `main`. Keep PRs small (ideally one sub-issue per PR).
2. **Commit** in conventional format (hook enforces locally).
3. **Push** and **open PR** — title must also be conventional (GHA enforces).
4. **Merge prerequisites (run in parallel; all three must clear):**
   - **CI green** — `pull_request` workflows fire on every PR open / push to the PR branch:
     - **Currently shipping**:
       - DCO sign-off (`.github/workflows/dco.yml`)
       - Conventional Commits PR title (`.github/workflows/pr-title.yml`)
     - **Planned in #0.5**: `cargo fmt --check`, `cargo clippy --release -- -D warnings`, `cargo test --release`, `pnpm check`, `pnpm lint` (the root scripts forward to `pnpm --filter web …` since `prettier` only lives in the `web` workspace)
     - **Planned in #2.10**: Lighthouse budget for frontend PRs (perf ≥ 85, a11y ≥ 95)
     - Run the planned commands locally as a pre-push habit until CI catches up
   - **Copilot first-pass review** — maintainers assign GitHub Copilot.
     Expect a 2–4-minute turnaround per round. Address comments you
     agree with (push fixes); reply with rationale on the ones you
     don't. Resolve threads when settled. Iterate until Copilot posts
     *"generated no new comments"*. Maintainers run the loop — you
     don't need to assign Copilot yourself.
   - **Human review** — at least one approving review from a maintainer.
5. **Squash-merge** — PR title becomes the merged commit subject; the
   maintainer composes a curated body that summarises what landed
   (the per-round commits get rolled up). Keeps `main` history linear
   and conventional.
6. **Delete branch** after merge.

## Where to look

| What | Where |
|---|---|
| Architecture & design | [`docs/DESIGN.md`](docs/DESIGN.md) |
| Roadmap & issues | [Project #2](https://github.com/users/hydai/projects/2) |
| AI assistant memory | [`CLAUDE.md`](CLAUDE.md) |
| Library version pins | [`docs/DESIGN.md` §5](docs/DESIGN.md#5-關鍵函式庫選擇版本驗證至-2026-05-18) |
| Critical gotchas | [`docs/DESIGN.md` §5.5](docs/DESIGN.md#55-major-bump-變動清單實作時務必注意) and `CLAUDE.md` |

## Quality bars

| Concern | Bar |
|---|---|
| Rust lints | `cargo clippy --release -- -D warnings` |
| Rust format | `cargo fmt --check` |
| Rust tests | `cargo test --release` pass (release builds only per project rule) |
| Frontend lint | `pnpm check` + `pnpm lint` (root scripts forward to `--filter web`) |
| Lighthouse (frontend) | perf ≥ 85, a11y ≥ 95, best-practices ≥ 90, SEO ≥ 90 |
| Security | Never log secrets; OAuth tokens stored AES-GCM encrypted; query_rows SQL via AST whitelist |
| Test coverage | No hard threshold — reviewer judgement; new business logic should have at least one happy-path and one edge-case test |

## Localization (i18n)

- **zh-TW is the source language.** Write zh-TW first, then translate.
- New UI strings go through Paraglide v2 (`getLocale()` / `setLocale()` / `locales`).
- DB i18n columns use `jsonb` shape `{"zh-TW": "…", "en": "…"}`; read with
  `COALESCE(col->>$lang, col->>'zh-TW')`.

## Security disclosures

Found a vulnerability? **Do NOT open a public issue.** Email
`security@taiwan-data-hub.example` (TBD) or use GitHub's private
vulnerability reporting:
https://github.com/hydai/taiwan-data-hub/security/advisories/new

We aim to acknowledge within 72 hours.

## Questions?

Open a [Discussion](https://github.com/hydai/taiwan-data-hub/discussions) —
they're enabled and prefer them to issues for open-ended conversations.

Thank you for contributing!
