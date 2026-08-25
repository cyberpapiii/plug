#!/usr/bin/env bash
set -euo pipefail

MODE="${1:-run}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="Plug"
PROJECT="$ROOT_DIR/PlugApp.xcodeproj"
DERIVED="$ROOT_DIR/.build"
APP="$DERIVED/Build/Products/Debug/Plug.app"

pkill -x "$APP_NAME" >/dev/null 2>&1 || true
xcodegen generate --spec "$ROOT_DIR/project.yml"
xcodebuild -project "$PROJECT" -scheme PlugApp -destination 'platform=macOS' \
  -derivedDataPath "$DERIVED" CODE_SIGNING_ALLOWED=NO build >/dev/null

open_app() { /usr/bin/open -n "$APP"; }

case "$MODE" in
  run) open_app ;;
  --debug|debug) lldb -- "$APP/Contents/MacOS/Plug" ;;
  --logs|logs) open_app; /usr/bin/log stream --info --style compact --predicate 'process == "Plug"' ;;
  --telemetry|telemetry) open_app; /usr/bin/log stream --info --style compact --predicate 'subsystem == "com.cyberpapiii.plug"' ;;
  --verify|verify) open_app; sleep 2; pgrep -x Plug >/dev/null ;;
  *) echo "usage: $0 [run|--debug|--logs|--telemetry|--verify]" >&2; exit 2 ;;
esac
