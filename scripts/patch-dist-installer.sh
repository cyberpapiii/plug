#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <cargo-dist-installer.sh>" >&2
  exit 2
fi

INSTALLER="$1"
[[ -f "$INSTALLER" ]] || {
  echo "dist installer not found: $INSTALLER" >&2
  exit 1
}

MARKER="# Plug publishes macOS through Plug.app, not this standalone installer."
if grep -Fq "$MARKER" "$INSTALLER"; then
  exit 0
fi

TEMP_FILE="$(mktemp "${INSTALLER}.tmp.XXXXXX")"
trap 'rm -f "$TEMP_FILE"' EXIT

awk -v marker="$MARKER" '
  !inserted && /^set -u$/ {
    print
    print ""
    print marker
    print "if [ \"$(uname -s)\" = \"Darwin\" ]; then"
    print "    cat >&2 <<\047EOF_MACOS\047"
    print "macOS standalone CLI artifacts are not published."
    print "Download and open Plug.app from the release DMG:"
    print "  https://github.com/cyberpapiii/plug/releases"
    print "Or install the app with Homebrew:"
    print "  brew install --cask cyberpapiii/tap/plug-app"
    print "EOF_MACOS"
    print "    exit 1"
    print "fi"
    inserted = 1
    next
  }
  { print }
  END {
    if (!inserted) {
      print "cargo-dist installer missing expected set -u marker" > "/dev/stderr"
      exit 1
    }
  }
' "$INSTALLER" > "$TEMP_FILE"

chmod +x "$TEMP_FILE"
mv "$TEMP_FILE" "$INSTALLER"
trap - EXIT
