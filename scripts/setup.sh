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

# Encourage DCO via -s by default — contributors can override if they wish.
if [[ "$(git config --get format.signOff || true)" != "true" ]]; then
  git config format.signOff true
  echo "  ✓ format.signOff = true (auto -s for git commit & format-patch)"
fi

# Helpful aliases
git config alias.cm 'commit'
git config alias.st 'status -sb'

echo ""
echo "✅ Setup complete. Next steps:"
echo "   • Read CLAUDE.md and CONTRIBUTING.md"
echo "   • Run \`gh auth setup-git\` if you push over HTTPS"
echo "   • When implementation starts: docker compose up -d"
