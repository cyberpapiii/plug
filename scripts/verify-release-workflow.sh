#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKFLOW="$ROOT/.github/workflows/release.yml"
DIST_CONFIG="$ROOT/dist-workspace.toml"

[[ -f "$WORKFLOW" ]] || { echo "release workflow missing: $WORKFLOW" >&2; exit 1; }

assert_contains() {
  local file="$1"
  local expected="$2"
  grep -Fq -- "$expected" "$file" || {
    echo "release workflow contract missing: $expected" >&2
    exit 1
  }
}

assert_absent() {
  local file="$1"
  local forbidden="$2"
  if grep -Fq -- "$forbidden" "$file"; then
    echo "release workflow contract contains forbidden text: $forbidden" >&2
    exit 1
  fi
}

# cargo-dist is used for artifact generation, while this workflow owns app
# signing and release publication. Keep that split explicit and reviewable.
assert_contains "$DIST_CONFIG" 'ci = []'
assert_absent "$DIST_CONFIG" 'allow-dirty'

for target in \
  'x86_64-unknown-linux-gnu' \
  'x86_64-unknown-linux-musl' \
  'aarch64-unknown-linux-gnu' \
  'aarch64-unknown-linux-musl'; do
  assert_contains "$WORKFLOW" "target: $target"
done

assert_contains "$WORKFLOW" 'cargo build --release -p plug-mcp --target aarch64-apple-darwin'
assert_contains "$WORKFLOW" 'cargo build --release -p plug-mcp --target x86_64-apple-darwin'
assert_contains "$WORKFLOW" './scripts/sign-notarize-macos-app.sh'
assert_contains "$WORKFLOW" 'dist build --artifacts=global --tag "$TAG"'
assert_contains "$WORKFLOW" './scripts/patch-dist-installer.sh target/distrib/plug-mcp-installer.sh'
assert_contains "$WORKFLOW" 'cp target/distrib/plug-mcp-installer.sh artifacts/'
assert_contains "$WORKFLOW" 'artifacts/plug-mcp-installer.sh'
assert_contains "$WORKFLOW" 'artifacts/Plug-*.dmg'

assert_absent "$WORKFLOW" '--allow-dirty'
assert_absent "$WORKFLOW" 'plug-mcp-aarch64-apple-darwin.tar.gz'
assert_absent "$WORKFLOW" 'plug-mcp-x86_64-apple-darwin.tar.gz'
assert_absent "$WORKFLOW" 'sign-macos-release.sh'

echo "Release workflow contract verified."
