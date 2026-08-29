#!/usr/bin/env bash
# Take a change from your working tree to merged, without any git plumbing.
#
#   scripts/ship.sh "fix: stop the daemon racing its own socket"
#   scripts/ship.sh          push and ship whatever is already committed here
#
# What it does: stage tracked edits, branch off main if you are on main, commit,
# push (which runs the pre-push gate), open a pull request, and turn on
# auto-merge. GitHub merges it when CI goes green and deletes the branch. You do
# not come back to it.
#
# It stages tracked modifications only, never untracked files. New files need an
# explicit `git add`. That boundary is deliberate: untracked files in this repo
# include private notes and local credentials, and a script that swept them in
# would eventually publish one.
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

message="${1:-}"

case "$message" in
  -h | --help)
    sed -n '2,17p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
esac

branch="$(git rev-parse --abbrev-ref HEAD)"

slug() {
  printf '%s' "$1" \
    | tr '[:upper:]' '[:lower:]' \
    | sed -e 's/^[a-z]*(\{0,1\}[a-z-]*)\{0,1\}: *//' \
    | tr -c 'a-z0-9' '-' \
    | sed -e 's/-\{2,\}/-/g' -e 's/^-//' -e 's/-$//' \
    | cut -c1-48
}

if [[ -n "$message" ]]; then
  git add -u
  if git diff --cached --quiet; then
    echo "ship: nothing staged. Tracked files are unchanged; new files need 'git add'." >&2
    exit 1
  fi

  if [[ "$branch" == "main" ]]; then
    branch="ship/$(slug "$message")"
    [[ "$branch" != "ship/" ]] || branch="ship/$(date +%Y%m%d-%H%M%S)"
    git checkout -q -b "$branch"
  fi

  echo "ship: committing to $branch"
  git --no-pager diff --cached --stat
  git commit -q -m "$message"
elif [[ "$branch" == "main" ]]; then
  echo "ship: on main with no message. Pass a commit message to start a branch." >&2
  exit 1
fi

if [[ -z "$(git log --oneline origin/main.."$branch" 2>/dev/null)" ]]; then
  echo "ship: $branch has no commits beyond origin/main. Nothing to ship." >&2
  exit 1
fi

git push -q -u origin "$branch"

url="$(gh pr view "$branch" --json url --jq .url 2>/dev/null || true)"
if [[ -z "$url" ]]; then
  title="$(git log -1 --pretty=%s)"
  body="$(git log -1 --pretty=%b)"
  url="$(gh pr create --base main --head "$branch" --title "$title" --body "${body:-$title}")"
fi

# --auto rather than a plain merge: the pull request lands only after CI agrees,
# and lands without anyone watching for it to finish.
gh pr merge "$branch" --squash --auto >/dev/null

echo
echo "ship: $url"
echo "ship: auto-merge armed. It merges itself when CI is green, then deletes the branch."
