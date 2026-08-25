#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REAL_CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
REAL_RUSTUP_HOME="${RUSTUP_HOME:-$(rustup show home)}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

TEST_HOME="$TMP/home"
TEST_CARGO_HOME="$TMP/cargo"
FAKE_BIN="$TMP/bin"
SECURITY_LOG="$TMP/security.log"
TEST_KEYCHAIN="$TMP/login.keychain-db"
mkdir -p "$TEST_HOME/.local/bin" "$TEST_CARGO_HOME/bin" "$FAKE_BIN"
: > "$TEST_KEYCHAIN"

# Keep dependency downloads out of the isolated install home while preserving
# an independent CARGO_HOME for every path the installer owns.
for cache in registry git; do
  if [[ -e "$REAL_CARGO_HOME/$cache" ]]; then
    ln -s "$REAL_CARGO_HOME/$cache" "$TEST_CARGO_HOME/$cache"
  fi
done

cat > "$FAKE_BIN/security" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$PLUG_TEST_SECURITY_LOG"
if [[ "${1:-}" == "find-identity" ]]; then
  echo '  1) FIXTURE "Plug Local Signing"'
  echo '     1 valid identities found'
fi
EOF

cat > "$FAKE_BIN/codesign" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "-dv" ]]; then
  echo 'Authority=Plug Local Signing' >&2
fi
exit 0
EOF
chmod +x "$FAKE_BIN/security" "$FAKE_BIN/codesign"

printf 'cargo-production-sentinel\n' > "$TEST_CARGO_HOME/bin/plug"
printf 'local-production-sentinel\n' > "$TEST_HOME/.local/bin/plug"
cargo_before="$(cksum "$TEST_CARGO_HOME/bin/plug")"
local_before="$(cksum "$TEST_HOME/.local/bin/plug")"

HOME="$TEST_HOME" \
CARGO_HOME="$TEST_CARGO_HOME" \
RUSTUP_HOME="$REAL_RUSTUP_HOME" \
CARGO_TARGET_DIR="$ROOT_DIR/target" \
PLUG_CODESIGN_KEYCHAIN="$TEST_KEYCHAIN" \
PLUG_TEST_SECURITY_LOG="$SECURITY_LOG" \
PATH="$FAKE_BIN:$PATH" \
  "$ROOT_DIR/scripts/dev-reinstall.sh" --quick

test -x "$TEST_CARGO_HOME/bin/plug-dev"
test "$cargo_before" = "$(cksum "$TEST_CARGO_HOME/bin/plug")"
test "$local_before" = "$(cksum "$TEST_HOME/.local/bin/plug")"
PLUG_DEV=1 "$TEST_CARGO_HOME/bin/plug-dev" --version >/dev/null

if [[ "$(uname -s)" == "Darwin" ]]; then
  grep -F "find-identity -v -p codesigning $TEST_KEYCHAIN" "$SECURITY_LOG" >/dev/null
fi

echo "PASS: plug-dev installed; production paths unchanged"
