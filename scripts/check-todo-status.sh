#!/usr/bin/env bash
# Fails if any todos/NNN-<status>-*.md filename disagrees with its frontmatter status.
set -euo pipefail
cd "$(dirname "$0")/.."
fail=0
for f in todos/[0-9]*.md; do
  base=$(basename "$f")
  tok=$(echo "$base" | cut -d- -f2)
  fm=$(grep -m1 '^status:' "$f" | awk '{print $2}' || true)
  if [ -z "$fm" ]; then echo "MISSING status: $base"; fail=1; continue; fi
  if [ "$fm" != "$tok" ]; then echo "MISMATCH: $base (name=$tok, frontmatter=$fm)"; fail=1; fi
done
exit $fail
