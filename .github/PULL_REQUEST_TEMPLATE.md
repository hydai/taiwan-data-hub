## Summary

<!--
One paragraph describing what this PR does and why. Reviewers should
be able to understand the change from this section alone.
-->

Closes #<!-- N -->.

## What's in this PR

<!-- Bulleted list of concrete changes. Optional but encouraged for
     PRs that touch multiple files / concerns. -->

## Notable choices

<!-- Anything a reviewer might disagree with: trade-offs, deferred
     work, deviations from docs/DESIGN.md, etc. -->

## Out of scope

<!-- What this PR does NOT do. Helps reviewers calibrate. -->

## Breaking changes

<!--
If this PR introduces a breaking change, mark the box below and
describe the migration path. Otherwise leave the box unchecked and
delete this section.
-->

- [ ] This PR introduces a breaking change (also mark the type with `!` in the title, e.g. `refactor(backend)!: …`)
- Migration notes (only if the box above is checked):

## Test plan

- [ ] `lineguard <changed files>` passes (format / line-endings)
- [ ] `cargo fmt --check` passes
- [ ] `cargo clippy --release --locked --all-targets -- -D warnings` passes
- [ ] `cargo test --release --locked` passes
- [ ] `pnpm check` passes (frontend touched? then required)
- [ ] `pnpm lint` passes (frontend touched? then required)
- [ ] `pnpm build` passes (frontend touched? then required)
- [ ] `docker compose -f docker/compose.yaml config` validates (compose touched? then required)
- [ ] Manual smoke test of the affected surface
- [ ] CI green
- [ ] Copilot review converges to "no new comments"

## Conventions checklist

- [ ] PR title follows Conventional Commits:
      `<type>: <subject>` — optional scope as `<type>(<scope>): <subject>` — optional `!` after the type/scope to mark a breaking change — optional trailing `(#<sub-issue-id>)` (e.g. `#0.7`, `#5a.2`)
- [ ] Branch name follows `<type>/<short-description>` or `<type>/<gh-issue>-<short-desc>`
- [ ] Every commit has `Signed-off-by:` (DCO) — `git commit -s` or our prepare-commit-msg hook does this automatically
- [ ] PR is attached to the correct milestone (M0 – M7 / M5a / M5b) and bears the right labels (component + estimate)
- [ ] Updated `docs/DESIGN.md` if architecture or scope changed
- [ ] Updated `CLAUDE.md` / `CONTRIBUTING.md` if conventions changed
