#!/usr/bin/env bash
# Run the CI lanes a change actually needs, locally, before pushing.
#
# Lane selection comes from scripts/classify-changes.sh, the same file the CI
# `classify` job runs. A green `dev.sh` and a red CI on the same commit means
# those rules changed, not that the two gates disagree by construction.
#
# Usage:
#   scripts/dev.sh                 lanes the working tree touches, with tests
#   scripts/dev.sh --quick         same lanes, formatting and lints only
#   scripts/dev.sh --base main     lanes for HEAD against a base ref
#   scripts/dev.sh --all           rust and app lanes regardless of what changed
#   scripts/dev.sh --list          print the selected lanes and exit
#
# The e2e lane needs `npm ci` and Playwright browsers. It is skipped unless
# explicitly requested with --e2e, --all included, because installing browsers
# is not a thing a pre-push check should do behind your back.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

quick=false
all=false
want_e2e=false
list_only=false
base=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --quick) quick=true ;;
    --all) all=true ;;
    --e2e) want_e2e=true ;;
    --list) list_only=true ;;
    --base)
      base="${2:?--base needs a ref}"
      shift
      ;;
    -h | --help)
      sed -n '2,17p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
  shift
done

if [[ "$all" == true ]]; then
  rust=true
  app=true
  e2e=true
else
  if [[ -n "$base" ]]; then
    eval "$(./scripts/classify-changes.sh "$base" HEAD)"
  else
    eval "$(./scripts/classify-changes.sh --working-tree)"
  fi
fi

# The e2e lane is opt-in even when the classifier selects it.
[[ "$want_e2e" == true ]] || e2e=false

if [[ "$list_only" == true ]]; then
  printf 'rust=%s app=%s e2e=%s\n' "$rust" "$app" "$e2e"
  exit 0
fi

if [[ "$rust" == false && "$app" == false && "$e2e" == false ]]; then
  echo "No validation lanes needed for these changes."
  exit 0
fi

failed=()

step() {
  local label="$1"
  shift
  local start
  start=$(date +%s)
  printf '\n\033[1m== %s\033[0m\n' "$label"
  if "$@"; then
    printf '\033[32mok\033[0m  %s (%ss)\n' "$label" "$(($(date +%s) - start))"
  else
    printf '\033[31mFAIL\033[0m %s (%ss)\n' "$label" "$(($(date +%s) - start))"
    failed+=("$label")
  fi
}

if [[ "$rust" == true ]]; then
  step "cargo fmt" cargo fmt --check
  step "cargo clippy" cargo clippy --workspace --all-targets -- -D warnings
  [[ "$quick" == true ]] || step "cargo test" cargo test --workspace
fi

if [[ "$app" == true ]]; then
  step "PlugApp architecture" ./scripts/check-app-architecture.sh
  [[ "$quick" == true ]] || step "PlugIPC tests" swift test --package-path PlugApp/PlugIPC
  [[ "$quick" == true ]] || step "PlugApp tests" ./scripts/test-app.sh
fi

if [[ "$e2e" == true ]]; then
  step "browser OAuth" npm run test:e2e
fi

"$repo_root/scripts/clean-build-artifacts.sh" --guard || true

if ((${#failed[@]} > 0)); then
  printf '\n\033[31m%d lane(s) failed:\033[0m %s\n' "${#failed[@]}" "${failed[*]}"
  exit 1
fi

printf '\n\033[32mAll selected lanes passed.\033[0m\n'
