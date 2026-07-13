#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CARGO_BIN_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"
LOCAL_BIN_DIR="$HOME/.local/bin"
CARGO_PLUG="$CARGO_BIN_DIR/plug"
LOCAL_PLUG="$LOCAL_BIN_DIR/plug"
STAGE_DIR=""

RUN_TESTS=1
CLEAN_AFTER=0

usage() {
  cat <<'EOF'
dev-reinstall.sh

Rebuild and reinstall the local `plug` binary in a way that avoids the
macOS copied-binary code-signing kill. The installed command on PATH is
normalized to a symlink pointing at ~/.cargo/bin/plug.

Usage:
  ./scripts/dev-reinstall.sh
  ./scripts/dev-reinstall.sh --quick
  ./scripts/dev-reinstall.sh --quick --clean

Options:
  --quick   Skip `cargo test -p plug-core`
  --clean   Remove generated build/deploy artifacts after reinstall
  -h        Show this help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --quick)
      RUN_TESTS=0
      shift
      ;;
    --clean)
      CLEAN_AFTER=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

cd "$ROOT_DIR"

cleanup() {
  if [[ -n "$STAGE_DIR" && -d "$STAGE_DIR" ]]; then
    rm -rf "$STAGE_DIR"
  fi
}
trap cleanup EXIT

echo "==> Checking workspace"
cargo check --workspace

if [[ "$RUN_TESTS" -eq 1 ]]; then
  echo "==> Running plug-core tests"
  cargo test -p plug-core
fi

mkdir -p "$CARGO_BIN_DIR"
STAGE_DIR="$(mktemp -d "$CARGO_BIN_DIR/.plug-install.XXXXXX")"
STAGED_PLUG="$STAGE_DIR/bin/plug"

echo "==> Building plug in a staging directory"
cargo install --path plug --root "$STAGE_DIR" --force --locked

# On macOS, re-sign with the stable self-signed identity so the Keychain
# "Always Allow" ACL persists across rebuilds (a bare install is ad-hoc signed,
# whose signature changes every build and re-triggers Keychain prompts).
# See scripts/setup-codesigning.sh and
# docs/solutions/integration-issues/local-codesigning-identity-stops-keychain-reprompts.md
if [[ "$(uname -s)" == "Darwin" ]]; then
  SIGN_IDENTITY="Plug Local Signing"
  if security find-identity -v -p codesigning 2>/dev/null | grep -qF "$SIGN_IDENTITY"; then
    echo "==> Code-signing staged plug with '$SIGN_IDENTITY'"
    codesign --force -s "$SIGN_IDENTITY" "$STAGED_PLUG"
    codesign --verify --deep --strict "$STAGED_PLUG"
    codesign -dv --verbose=2 "$STAGED_PLUG" 2>&1 | grep -E 'Authority' || true
  else
    echo "error: '$SIGN_IDENTITY' identity not found; refusing to replace the signed binary." >&2
    echo "       Run ./scripts/setup-codesigning.sh, then retry this install." >&2
    exit 1
  fi
fi

echo "==> Smoke testing staged binary"
"$STAGED_PLUG" --help >/dev/null

echo "==> Atomically installing verified plug to $CARGO_PLUG"
mv -f "$STAGED_PLUG" "$CARGO_PLUG"

mkdir -p "$LOCAL_BIN_DIR"

if [[ -L "$LOCAL_PLUG" ]]; then
  current_target="$(readlink "$LOCAL_PLUG" || true)"
  if [[ "$current_target" != "$CARGO_PLUG" ]]; then
    rm -f "$LOCAL_PLUG"
    ln -s "$CARGO_PLUG" "$LOCAL_PLUG"
  fi
elif [[ -e "$LOCAL_PLUG" ]]; then
  rm -f "$LOCAL_PLUG"
  ln -s "$CARGO_PLUG" "$LOCAL_PLUG"
else
  ln -s "$CARGO_PLUG" "$LOCAL_PLUG"
fi

echo "==> Smoke testing installed binary"
"$CARGO_PLUG" --help >/dev/null
"$LOCAL_PLUG" --help >/dev/null

if [[ "$CLEAN_AFTER" -eq 1 ]]; then
  echo "==> Cleaning generated build artifacts"
  "$ROOT_DIR/scripts/clean-build-artifacts.sh" --yes
fi

echo
echo "Installed:"
echo "  cargo bin: $CARGO_PLUG"
echo "  path bin:  $LOCAL_PLUG -> $(readlink "$LOCAL_PLUG")"
echo
echo "Run:"
echo "  plug"
