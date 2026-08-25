#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GENERATOR="$ROOT/scripts/generate-release-metadata.sh"
FIXTURE_ROOT="$(mktemp -d)"
trap 'rm -rf "$FIXTURE_ROOT"' EXIT

OUTPUT="$FIXTURE_ROOT/metadata"
mkdir -p "$OUTPUT"

WORKSPACE_VERSION="$(cargo metadata --manifest-path "$ROOT/Cargo.toml" --no-deps --format-version 1 \
    | python3 -c 'import json,sys; data=json.load(sys.stdin); print(next(p["version"] for p in data["packages"] if p["name"] == "plug-mcp"))')"
VERSION="1.2.3"
DMG_SHA="dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
LINUX_ARM_SHA="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
LINUX_X64_SHA="bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

assert_contains() {
    local file="$1"
    local expected="$2"
    if ! grep -Fq -- "$expected" "$file"; then
        echo "FAIL: $file missing: $expected" >&2
        exit 1
    fi
}

assert_absent() {
    local file="$1"
    local unexpected="$2"
    if grep -Fq -- "$unexpected" "$file"; then
        echo "FAIL: $file contains forbidden text: $unexpected" >&2
        exit 1
    fi
}

bash "$ROOT/scripts/verify-release-workflow.sh"

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

DIST_INSTALLER="$ROOT/target/distrib/plug-mcp-installer.sh"
command -v dist >/dev/null 2>&1 || {
    echo "FAIL: cargo-dist is required to test published installer contract" >&2
    exit 1
}
(cd "$ROOT" && dist build --artifacts=global --tag "v$WORKSPACE_VERSION" >/dev/null)
[[ -f "$DIST_INSTALLER" ]] || {
    echo "FAIL: cargo-dist installer not generated" >&2
    exit 1
}
bash "$ROOT/scripts/patch-dist-installer.sh" "$DIST_INSTALLER"
[[ -x "$DIST_INSTALLER" ]] || {
    echo "FAIL: published installer patch did not preserve executable contract" >&2
    exit 1
}
assert_contains "$DIST_INSTALLER" \
    '# Plug publishes macOS through Plug.app, not this standalone installer.'

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
mkdir -p "$FAKE_BIN" "$FIXTURE_ROOT/home" "$FIXTURE_ROOT/config"
CALL_MARKER="$FIXTURE_ROOT/network-called"
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
cat > "$FAKE_BIN/wget" <<EOF
#!/bin/sh
touch "$CALL_MARKER"
exit 97
EOF
chmod +x "$FAKE_BIN/uname" "$FAKE_BIN/curl" "$FAKE_BIN/wget"

run_darwin_refusal() {
    local installer="$1"
    local label="$2"
    local install_output
    local install_status
    set +e
    install_output="$(PATH="$FAKE_BIN:$PATH" HOME="$FIXTURE_ROOT/home" \
        XDG_CONFIG_HOME="$FIXTURE_ROOT/config" sh "$installer" 2>&1)"
    install_status=$?
    set -e
    if [[ "$install_status" -eq 0 ]]; then
        echo "FAIL: Darwin $label unexpectedly succeeded" >&2
        exit 1
    fi
    if [[ -e "$CALL_MARKER" ]]; then
        echo "FAIL: Darwin $label attempted a network download" >&2
        exit 1
    fi
    if [[ "$install_output" != *"Plug.app"* || \
          "$install_output" != *"DMG"* || \
          "$install_output" != *"brew install --cask cyberpapiii/tap/plug-app"* ]]; then
        echo "FAIL: Darwin $label lacks Plug.app DMG/Cask guidance" >&2
        echo "$install_output" >&2
        exit 1
    fi
    if [[ -n "$(find "$FIXTURE_ROOT/home" "$FIXTURE_ROOT/config" \
        -mindepth 1 -print -quit)" ]]; then
        echo "FAIL: Darwin $label mutated install state before refusal" >&2
        exit 1
    fi
}

run_darwin_refusal "$DIST_INSTALLER" "cargo-dist installer"
run_darwin_refusal "$ROOT/install.sh" "source installer"

echo "All release metadata contract tests passed."
