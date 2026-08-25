#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

TEST_HOME="$TMP/home"
TEST_CARGO_HOME="$TMP/cargo"
TEST_SIGNING_DIR="$TMP/signing"
TEST_KEYCHAIN="$TMP/selected.keychain-db"
FAKE_BIN="$TMP/bin"
SECURITY_LOG="$TMP/security.log"
SECURITY_STATE="$TMP/security-state"
CODESIGN_LOG="$TMP/codesign.log"
mkdir -p "$TEST_HOME" "$TEST_CARGO_HOME/bin" "$FAKE_BIN"
: > "$TEST_KEYCHAIN"
: > "$SECURITY_LOG"
: > "$SECURITY_STATE"
: > "$CODESIGN_LOG"
: > "$TEST_CARGO_HOME/bin/plug-dev"
chmod +x "$TEST_CARGO_HOME/bin/plug-dev"

cat > "$FAKE_BIN/security" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$PLUG_TEST_SECURITY_LOG"
if [[ "${1:-}" == "find-identity" ]]; then
  count="$(wc -l < "$PLUG_TEST_SECURITY_STATE" | tr -d ' ')"
  printf 'seen\n' >> "$PLUG_TEST_SECURITY_STATE"
  if [[ "$count" -gt 0 ]]; then
    echo '  1) FIXTURE "Plug Local Signing"'
    echo '     1 valid identities found'
  else
    echo '     0 valid identities found'
  fi
fi
EOF

cat > "$FAKE_BIN/openssl" <<'EOF'
#!/usr/bin/env bash
while [[ $# -gt 0 ]]; do
  case "$1" in
    -keyout|-out)
      shift
      mkdir -p "$(dirname "$1")"
      : > "$1"
      ;;
  esac
  shift
done
EOF

cat > "$FAKE_BIN/codesign" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$PLUG_TEST_CODESIGN_LOG"
if [[ "${1:-}" == "-dv" ]]; then
  echo 'Authority=Plug Local Signing' >&2
fi
exit 0
EOF
chmod +x "$FAKE_BIN/security" "$FAKE_BIN/openssl" "$FAKE_BIN/codesign"

HOME="$TEST_HOME" \
CARGO_HOME="$TEST_CARGO_HOME" \
PLUG_SIGNING_DIR="$TEST_SIGNING_DIR" \
PLUG_CODESIGN_KEYCHAIN="$TEST_KEYCHAIN" \
PLUG_TEST_SECURITY_LOG="$SECURITY_LOG" \
PLUG_TEST_SECURITY_STATE="$SECURITY_STATE" \
PLUG_TEST_CODESIGN_LOG="$CODESIGN_LOG" \
PATH="$FAKE_BIN:$PATH" \
  "$ROOT_DIR/scripts/setup-codesigning.sh"

grep -F "find-identity -v -p codesigning $TEST_KEYCHAIN" "$SECURITY_LOG" >/dev/null
grep -F "import $TEST_SIGNING_DIR/plug-signing.p12 -k $TEST_KEYCHAIN" "$SECURITY_LOG" >/dev/null
grep -F "add-trusted-cert -r trustRoot -p codeSign -k $TEST_KEYCHAIN $TEST_SIGNING_DIR/cert.pem" "$SECURITY_LOG" >/dev/null
grep -F -- "--force -s Plug Local Signing --keychain $TEST_KEYCHAIN $TEST_CARGO_HOME/bin/plug-dev" "$CODESIGN_LOG" >/dev/null

echo "PASS: setup uses the selected keychain for discovery, import, trust, and signing"
