#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/packaging/mcpb/manifest.json"
ASSETS_DIR="$ROOT/docs/assets"
MCPB_ASSETS_DIR="$ROOT/packaging/mcpb/assets"
BUILD_DIR="$ROOT/target/mcpb/plug"
OUT_DIR="$ROOT/target/dist"
BINARY="$ROOT/target/release/plug"
CHECK_ONLY=0

usage() {
  printf 'Usage: %s [--check] [--binary PATH] [--out-dir PATH]\n' "$0"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --check)
      CHECK_ONLY=1
      shift
      ;;
    --binary)
      if [ "$#" -lt 2 ]; then
        usage >&2
        exit 2
      fi
      BINARY="$2"
      shift 2
      ;;
    --out-dir)
      if [ "$#" -lt 2 ]; then
        usage >&2
        exit 2
      fi
      OUT_DIR="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
done

require_file() {
  if [ ! -f "$1" ]; then
    printf 'Missing required file: %s\n' "$1" >&2
    exit 1
  fi
}

validate_manifest() {
  if command -v mcpb >/dev/null 2>&1; then
    mcpb validate "$MANIFEST"
  elif command -v npx >/dev/null 2>&1; then
    npx --yes @anthropic-ai/mcpb validate "$MANIFEST"
  else
    printf 'mcpb or npx is required to validate the MCPB manifest\n' >&2
    exit 1
  fi
}

require_file "$MANIFEST"
for size in 16 32 64 128 256 512; do
  require_file "$ASSETS_DIR/plug-icon-${size}.png"
  require_file "$MCPB_ASSETS_DIR/plug-icon-${size}.png"
done

python3 -m json.tool "$MANIFEST" >/dev/null
validate_manifest

if [ "$CHECK_ONLY" -eq 1 ]; then
  printf 'MCPB inputs OK\n'
  exit 0
fi

if ! command -v mcpb >/dev/null 2>&1; then
  printf 'mcpb CLI is required to pack the bundle. Install it with: npm install -g @anthropic-ai/mcpb\n' >&2
  exit 1
fi

require_file "$BINARY"
if [ ! -x "$BINARY" ]; then
  printf 'Binary is not executable: %s\n' "$BINARY" >&2
  exit 1
fi
if ! "$BINARY" --version | grep -Eq '^plug [0-9]'; then
  printf 'Binary does not identify as plug: %s\n' "$BINARY" >&2
  exit 1
fi

rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR/bin" "$BUILD_DIR/assets" "$OUT_DIR"
cp "$MANIFEST" "$BUILD_DIR/manifest.json"
cp "$BINARY" "$BUILD_DIR/bin/plug"
chmod +x "$BUILD_DIR/bin/plug"

for size in 16 32 64 128 256 512; do
  cp "$MCPB_ASSETS_DIR/plug-icon-${size}.png" "$BUILD_DIR/assets/"
done

mcpb pack "$BUILD_DIR" "$OUT_DIR/plug.mcpb"
printf 'Built %s\n' "$OUT_DIR/plug.mcpb"
