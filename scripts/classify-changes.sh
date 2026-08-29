#!/usr/bin/env bash
# Decide which validation lanes a change set needs.
#
# One implementation, two callers: the CI `classify` job and the local
# `scripts/dev.sh` gate. Keeping it in one file is deliberate. A second copy
# of these rules would drift, and the local gate would start disagreeing with
# the gate that actually blocks a merge.
#
# Usage:
#   scripts/classify-changes.sh <base-ref> [head-ref]
#   scripts/classify-changes.sh --working-tree
#
# Prints three `key=value` lines on stdout:
#   rust=true|false
#   app=true|false
#   e2e=true|false
set -euo pipefail

emit() {
  printf 'rust=%s\napp=%s\ne2e=%s\n' "$1" "$2" "$3"
}

paths_from_working_tree() {
  git diff --name-only --diff-filter=ACMRT HEAD
  git ls-files --others --exclude-standard
}

paths_from_range() {
  local base="$1" head="$2"
  git diff --name-only --diff-filter=ACMRT "$base" "$head"
}

# `${1-...}` and not `${1:-...}`: an explicitly empty first argument is CI
# handing over an unknown base. That must fall through to the fail-safe below
# and run every lane, never to working-tree mode against a clean checkout.
mode="${1---working-tree}"
base=""
head=""

case "$mode" in
  --working-tree) ;;
  -h | --help)
    sed -n '2,16p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  *)
    base="$mode"
    head="${2:-HEAD}"
    # Fail safe: an unknown base runs every lane rather than skipping one.
    if [[ -z "$base" ]] || ! git cat-file -e "${base}^{commit}" 2>/dev/null; then
      emit true true true
      exit 0
    fi
    ;;
esac

changed_paths() {
  if [[ -n "$base" ]]; then
    paths_from_range "$base" "$head"
  else
    paths_from_working_tree
  fi
}

rust=false
app=false
e2e=false

while IFS= read -r path; do
  [[ -n "$path" ]] || continue
  case "$path" in
    docs/* | *.md) ;;
    PlugApp/*) app=true ;;
    testdata/ipc/* | testdata/legacy_plug_programs.json)
      rust=true
      app=true
      ;;
    package.json | package-lock.json | playwright.config.* | e2e/* | tests/e2e/*)
      e2e=true
      ;;
    plug-core/src/downstream_oauth/* | plug-core/src/http/* | plug-core/src/oauth.rs)
      rust=true
      e2e=true
      ;;
    scripts/check-app-architecture.sh | scripts/test-app.sh) app=true ;;
    .github/workflows/* | scripts/classify-changes.sh)
      rust=true
      app=true
      e2e=true
      ;;
    *) rust=true ;;
  esac
done < <(changed_paths)

emit "$rust" "$app" "$e2e"
