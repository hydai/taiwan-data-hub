#!/usr/bin/env bash
#
# One-shot dev-environment setup for Taiwan Data Hub.
# Idempotent — safe to re-run.
#
# Usage:  ./scripts/setup.sh

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

echo "→ Configuring git for this repo…"

git config core.hooksPath .githooks
chmod +x .githooks/* 2>/dev/null || true
echo "  ✓ commit-msg hook activated (.githooks/commit-msg)"

git config commit.template .gitmessage
echo "  ✓ commit template set (.gitmessage)"

# Auto DCO via the prepare-commit-msg hook (real, hook-driven mechanism).
# format.signOff still covers format-patch for patch-via-email workflows.
if [ "$(git config --get format.signOff || true)" != "true" ]; then
  git config format.signOff true
  echo "  ✓ format.signOff = true (auto -s for format-patch)"
fi
# Clear the bogus commit.signOff knob if it was set by a previous setup —
# git does not actually honor this config.
if [ -n "$(git config --get commit.signOff || true)" ]; then
  git config --unset commit.signOff || true
  echo "  ✓ removed obsolete commit.signOff config"
fi

# Helpful aliases
git config alias.cm 'commit'
git config alias.st 'status -sb'

echo ""
echo "✅ Setup complete. Next steps:"
echo "   • Read CLAUDE.md and CONTRIBUTING.md"
echo "   • Run \`gh auth setup-git\` if you push over HTTPS"
echo "   • When implementation starts: docker compose up -d"
