#!/usr/bin/env bash
# Wire up this clone so the day-to-day workflow needs no maintenance.
#
#   scripts/setup-dev.sh
#
# Idempotent. Run it on a fresh clone, or any time you want to confirm the
# workflow is still wired the way it should be.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

ok() { printf '\033[32m ok \033[0m %s\n' "$1"; }
note() { printf '\033[33mnote\033[0m %s\n' "$1"; }

current_hooks="$(git config --get core.hooksPath || true)"
if [[ "$current_hooks" == ".githooks" ]]; then
  ok "git hooks already point at .githooks"
else
  git config core.hooksPath .githooks
  ok "git hooks now point at .githooks"
fi
note "pre-push runs the quick gate; post-commit, post-merge and post-checkout run the artifact guard"

if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
  settings="$(gh api "repos/${GITHUB_REPOSITORY:-cyberpapiii/plug}" \
    --jq '"\(.allow_auto_merge) \(.delete_branch_on_merge)"' 2>/dev/null || echo "unknown unknown")"
  case "$settings" in
    "true true") ok "GitHub auto-merge and branch auto-delete are on" ;;
    "unknown unknown") note "could not read GitHub repo settings; skipping that check" ;;
    *) note "GitHub auto-merge / branch auto-delete are off. scripts/ship.sh needs them: gh api -X PATCH repos/cyberpapiii/plug -f allow_auto_merge=true -F delete_branch_on_merge=true" ;;
  esac
else
  note "gh is not installed or not logged in; scripts/ship.sh needs it"
fi

if command -v xcodegen >/dev/null 2>&1; then
  ok "xcodegen is installed, so the PlugApp lane can run"
else
  note "xcodegen is missing. The PlugApp lane in scripts/dev.sh needs it: brew install xcodegen"
fi

"$repo_root/scripts/clean-build-artifacts.sh" --guard || true
ok "artifact guard checked (silent means everything is inside budget)"

cat <<'NEXT'

Day to day:
  scripts/dev.sh              run the checks this change needs
  scripts/ship.sh "message"   commit, push, open a pull request, auto-merge

Everything else looks after itself. The artifact guard runs on every commit,
merge, checkout and push, and stays quiet unless something has grown.
NEXT
