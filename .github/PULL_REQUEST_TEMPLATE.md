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

## Test plan

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

- [ ] PR title is Conventional Commits (`<type>(<scope>)?: <subject> (#<sub-issue-id>)?`)
- [ ] Branch name follows `<type>/<short-description>` or `<type>/<gh-issue>-<short-desc>`
- [ ] Every commit has `Signed-off-by:` (DCO) — `git commit -s` or our prepare-commit-msg hook does this automatically
- [ ] Updated `docs/DESIGN.md` if architecture or scope changed
- [ ] Updated `CLAUDE.md` / `CONTRIBUTING.md` if conventions changed
