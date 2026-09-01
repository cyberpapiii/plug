#!/usr/bin/env bash
# Take a finished change all the way to a release installed on this Mac.
#
#   scripts/release.sh            release the next patch version
#   scripts/release.sh 0.9.0      release a specific version
#
# One command covers the whole path: version bump, changelog, pull request,
# auto-merge, tag, the release build, the signed install, and the build-cache
# sweep afterwards. It waits for each stage rather than handing back a set of
# steps to run later.
#
# It ships tracked modifications the way scripts/ship.sh does and never stages
# untracked files. Write the changelog entries under `## [Unreleased]` before
# running it; this script renames that heading to the version and refuses to
# invent release notes.
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

case "${1:-}" in
  -h | --help)
    sed -n '2,17p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
esac

workspace_version() {
  cargo metadata --no-deps --format-version 1 |
    python3 -c 'import json,sys; d=json.load(sys.stdin); print(next(p["version"] for p in d["packages"] if p["name"] == "plug-mcp"))'
}

current="$(workspace_version)"
version="${1:-}"
if [[ -z "$version" ]]; then
  version="$(python3 -c 'import sys; a=sys.argv[1].split("."); a[2]=str(int(a[2])+1); print(".".join(a))' "$current")"
fi
tag="v$version"

if gh release view "$tag" >/dev/null 2>&1; then
  echo "release: $tag is already published" >&2
  exit 1
fi

echo "release: $current -> $version"

if [[ "$current" != "$version" ]]; then
  # The workspace carries one version line; every crate inherits it.
  python3 - "$version" <<'PY'
import re, sys
version = sys.argv[1]
path = "Cargo.toml"
text = open(path).read()
updated, count = re.subn(r'(?m)^version = "[^"]+"$', f'version = "{version}"', text, count=1)
if count != 1:
    raise SystemExit("release: could not find the workspace version line in Cargo.toml")
open(path, "w").write(updated)
PY
  cargo update --workspace --quiet

  python3 - "$version" <<'PY'
import datetime, sys
version = sys.argv[1]
path = "CHANGELOG.md"
text = open(path).read()
heading = "## [Unreleased]"
if heading not in text:
    raise SystemExit(
        "release: CHANGELOG.md has no '## [Unreleased]' section. "
        "Write the release notes there first."
    )
today = datetime.date.today().isoformat()
open(path, "w").write(text.replace(heading, f"## [{version}] - {today}", 1))
PY
fi

if ! git diff --quiet || ! git diff --cached --quiet; then
  scripts/ship.sh "chore: prepare Plug $version release"
fi

branch="$(git rev-parse --abbrev-ref HEAD)"
if [[ "$branch" != "main" ]]; then
  echo "release: waiting for $branch to merge"
  for _ in $(seq 1 180); do
    state="$(gh pr view "$branch" --json state --jq .state 2>/dev/null || true)"
    case "$state" in
      MERGED) break ;;
      CLOSED) echo "release: $branch was closed without merging" >&2; exit 1 ;;
    esac
    sleep 20
  done
  [[ "$(gh pr view "$branch" --json state --jq .state 2>/dev/null || true)" == "MERGED" ]] ||
    { echo "release: $branch did not merge within an hour" >&2; exit 1; }
  git checkout -q main
fi

git pull -q --ff-only
git fetch -q --prune origin

merged="$(workspace_version)"
[[ "$merged" == "$version" ]] ||
  { echo "release: main carries $merged, expected $version" >&2; exit 1; }

echo "release: tagging $tag"
git tag "$tag"
git push -q origin "$tag"

# The tag push starts the release workflow. Give GitHub a moment to create the
# run before asking for it by name.
run=""
for _ in $(seq 1 30); do
  run="$(gh run list --workflow release.yml --branch "$tag" --limit 1 --json databaseId --jq '.[0].databaseId' 2>/dev/null || true)"
  [[ -z "$run" ]] || break
  sleep 5
done
[[ -n "$run" ]] || { echo "release: no release run appeared for $tag" >&2; exit 1; }

echo "release: watching run $run"
gh run watch "$run" --exit-status

scripts/install-release.sh "$version"


echo
echo "release: $tag published and installed"
gh release view "$tag" --json url --jq .url
