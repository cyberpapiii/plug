#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GENERATOR="$ROOT/scripts/generate-release-metadata.sh"
FIXTURE_ROOT="$(mktemp -d)"
trap 'rm -rf "$FIXTURE_ROOT"' EXIT

OUTPUT="$FIXTURE_ROOT/metadata"
mkdir -p "$OUTPUT"

VERSION="1.2.3"
DMG_SHA="dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
LINUX_ARM_SHA="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
LINUX_X64_SHA="bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

assert_contains() {
    local file="$1"
    local expected="$2"
    if ! grep -Fq "$expected" "$file"; then
        echo "FAIL: $file missing: $expected" >&2
        exit 1
    fi
}

assert_absent() {
    local file="$1"
    local unexpected="$2"
    if grep -Fq "$unexpected" "$file"; then
        echo "FAIL: $file contains forbidden text: $unexpected" >&2
        exit 1
    fi
}

bash "$GENERATOR" \
    --version "$VERSION" \
    --dmg-sha "$DMG_SHA" \
    --linux-arm-sha "$LINUX_ARM_SHA" \
    --linux-x64-sha "$LINUX_X64_SHA" \
    --output "$OUTPUT"

FORMULA="$OUTPUT/plug.rb"
CASK="$OUTPUT/plug-app.rb"
[[ -f "$FORMULA" ]] || { echo "FAIL: formula not generated" >&2; exit 1; }
[[ -f "$CASK" ]] || { echo "FAIL: Cask not generated" >&2; exit 1; }

assert_contains "$FORMULA" 'depends_on :linux'
assert_contains "$FORMULA" "plug-mcp-aarch64-unknown-linux-gnu.tar.gz"
assert_contains "$FORMULA" "plug-mcp-x86_64-unknown-linux-gnu.tar.gz"
assert_contains "$FORMULA" "$LINUX_ARM_SHA"
assert_contains "$FORMULA" "$LINUX_X64_SHA"
assert_absent "$FORMULA" 'apple-darwin'
assert_absent "$FORMULA" 'on_macos'
assert_absent "$FORMULA" '.dmg'
if [[ "$(grep -Ec '^[[:space:]]+url ' "$FORMULA")" -ne 2 ]]; then
    echo "FAIL: formula must contain exactly two Linux URLs" >&2
    exit 1
fi

assert_contains "$CASK" 'cask "plug-app" do'
assert_contains "$CASK" 'version "1.2.3"'
assert_contains "$CASK" "$DMG_SHA"
assert_contains "$CASK" 'Plug-1.2.3.dmg'
assert_contains "$CASK" 'app "Plug.app"'
assert_absent "$CASK" 'apple-darwin'
assert_absent "$CASK" 'binary '
assert_absent "$CASK" 'bin.install'

assert_absent "$ROOT/dist-workspace.toml" 'apple-darwin'
assert_absent "$ROOT/.github/workflows/release.yml" 'plug-mcp-aarch64-apple-darwin.tar.gz'
assert_absent "$ROOT/.github/workflows/release.yml" 'plug-mcp-x86_64-apple-darwin.tar.gz'
assert_absent "$ROOT/.github/workflows/release.yml" 'sign-macos-release.sh'
assert_contains "$ROOT/.github/workflows/release.yml" \
    'cargo build --release -p plug-mcp --target aarch64-apple-darwin'
assert_contains "$ROOT/.github/workflows/release.yml" \
    'cargo build --release -p plug-mcp --target x86_64-apple-darwin'
[[ ! -e "$ROOT/scripts/sign-macos-release.sh" ]] || {
    echo "FAIL: obsolete macOS signing script still exists" >&2
    exit 1
}

FAKE_BIN="$FIXTURE_ROOT/bin"
mkdir -p "$FAKE_BIN" "$FIXTURE_ROOT/home"
CALL_MARKER="$FIXTURE_ROOT/curl-called"
cat > "$FAKE_BIN/uname" <<'EOF'
#!/bin/sh
if [ "${1:-}" = "-s" ]; then
    echo Darwin
else
    /usr/bin/uname "$@"
fi
EOF
cat > "$FAKE_BIN/curl" <<EOF
#!/bin/sh
touch "$CALL_MARKER"
exit 97
EOF
chmod +x "$FAKE_BIN/uname" "$FAKE_BIN/curl"

set +e
INSTALL_OUTPUT="$(PATH="$FAKE_BIN:$PATH" HOME="$FIXTURE_ROOT/home" sh "$ROOT/install.sh" \
    --version "$VERSION" 2>&1)"
INSTALL_STATUS=$?
set -e
if [[ "$INSTALL_STATUS" -eq 0 ]]; then
    echo "FAIL: Darwin shell installer unexpectedly succeeded" >&2
    exit 1
fi
if [[ -e "$CALL_MARKER" ]]; then
    echo "FAIL: Darwin shell installer attempted a network download" >&2
    exit 1
fi
if [[ "$INSTALL_OUTPUT" != *"Plug.app"* || \
      "$INSTALL_OUTPUT" != *"DMG"* || \
      "$INSTALL_OUTPUT" != *"brew install --cask cyberpapiii/tap/plug-app"* ]]; then
    echo "FAIL: Darwin refusal lacks Plug.app DMG/Cask guidance" >&2
    echo "$INSTALL_OUTPUT" >&2
    exit 1
fi

echo "All release metadata contract tests passed."
