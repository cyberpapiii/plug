#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 --tag <vX.Y.Z> --app <Plug.app> --appcast <appcast.xml> --cask <plug-app.rb>" >&2
  exit 2
}

TAG=""
APP=""
APPCAST=""
CASK=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag) TAG="${2:-}"; shift 2 ;;
    --app) APP="${2:-}"; shift 2 ;;
    --appcast) APPCAST="${2:-}"; shift 2 ;;
    --cask) CASK="${2:-}"; shift 2 ;;
    *) usage ;;
  esac
done

[[ -n "$TAG" && -n "$APP" && -n "$APPCAST" && -n "$CASK" ]] || usage

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INFO_PLIST="$APP/Contents/Info.plist"
EMBEDDED_PLUG="$APP/Contents/Resources/plug"

[[ -f "$INFO_PLIST" ]] || { echo "release contract: app Info.plist not found: $INFO_PLIST" >&2; exit 1; }
[[ -x "$EMBEDDED_PLUG" ]] || { echo "release contract: embedded plug not executable: $EMBEDDED_PLUG" >&2; exit 1; }
[[ -f "$APPCAST" ]] || { echo "release contract: appcast not found: $APPCAST" >&2; exit 1; }
[[ -f "$CASK" ]] || { echo "release contract: Cask not found: $CASK" >&2; exit 1; }

WORKSPACE_VERSION="$(cargo metadata --manifest-path "$ROOT/Cargo.toml" --no-deps --format-version 1 \
  | python3 -c 'import json,sys; data=json.load(sys.stdin); print(next(p["version"] for p in data["packages"] if p["name"] == "plug-mcp"))')"
TAG_VERSION="${TAG#v}"
if [[ "$TAG" != v* || "$TAG_VERSION" != "$WORKSPACE_VERSION" ]]; then
  echo "release contract: tag version '$TAG_VERSION' does not match workspace version '$WORKSPACE_VERSION'" >&2
  exit 1
fi

read -r APP_VERSION APP_BUILD < <(python3 - "$INFO_PLIST" <<'PY'
import plistlib
import sys

with open(sys.argv[1], "rb") as handle:
    info = plistlib.load(handle)
print(info.get("CFBundleShortVersionString", ""), info.get("CFBundleVersion", ""))
PY
)

if [[ "$APP_VERSION" != "$WORKSPACE_VERSION" ]]; then
  echo "release contract: CFBundleShortVersionString '$APP_VERSION' does not match workspace version '$WORKSPACE_VERSION'" >&2
  exit 1
fi
if [[ ! "$APP_BUILD" =~ ^[0-9]+$ ]]; then
  echo "release contract: CFBundleVersion '$APP_BUILD' is not a numeric build number" >&2
  exit 1
fi

BINARY_OUTPUT="$($EMBEDDED_PLUG --version)"
BINARY_VERSION="${BINARY_OUTPUT##* }"
if [[ "$BINARY_VERSION" != "$WORKSPACE_VERSION" ]]; then
  echo "release contract: embedded plug version '$BINARY_VERSION' does not match workspace version '$WORKSPACE_VERSION'" >&2
  exit 1
fi

read -r APPCAST_VERSION APPCAST_BUILD PRIOR_MAX < <(python3 - "$APPCAST" "$WORKSPACE_VERSION" "$APP_BUILD" <<'PY'
import sys
import xml.etree.ElementTree as ET

path, expected_version, expected_build = sys.argv[1:]
root = ET.parse(path).getroot()

def attr(element, local_name):
    for key, value in element.attrib.items():
        if key == local_name or key.endswith("}" + local_name):
            return value
    return ""

entries = []
for enclosure in root.iter("enclosure"):
    short_version = attr(enclosure, "shortVersionString")
    build = attr(enclosure, "version")
    if short_version and build:
        entries.append((short_version, build))

current = next((entry for entry in entries if entry == (expected_version, expected_build)), None)
if current is None:
    same_version = next((entry for entry in entries if entry[0] == expected_version), None)
    if same_version is not None:
        print(same_version[0], same_version[1], -1)
    elif entries:
        print(entries[0][0], entries[0][1], -1)
    else:
        print("-", "-", -1)
    raise SystemExit(0)

prior_builds = [int(build) for version, build in entries
                if (version, build) != current and build.isdigit()]
print(current[0], current[1], max(prior_builds, default=-1))
PY
)

if [[ "$APPCAST_VERSION" != "$WORKSPACE_VERSION" ]]; then
  echo "release contract: appcast short version '$APPCAST_VERSION' does not match workspace version '$WORKSPACE_VERSION'" >&2
  exit 1
fi
if [[ "$APPCAST_BUILD" != "$APP_BUILD" ]]; then
  echo "release contract: appcast build '$APPCAST_BUILD' does not match CFBundleVersion '$APP_BUILD'" >&2
  exit 1
fi
if (( APP_BUILD <= PRIOR_MAX )); then
  echo "release contract: CFBundleVersion '$APP_BUILD' must be greater than prior appcast build '$PRIOR_MAX'" >&2
  exit 1
fi

CASK_VERSION="$(python3 - "$CASK" <<'PY'
import re
import sys

text = open(sys.argv[1], encoding="utf-8").read()
match = re.search(r'^\s*version\s+["\x27]([^"\x27]+)["\x27]\s*$', text, re.MULTILINE)
print(match.group(1) if match else "")
PY
)"
if [[ "$CASK_VERSION" != "$WORKSPACE_VERSION" ]]; then
  echo "release contract: Cask version '$CASK_VERSION' does not match workspace version '$WORKSPACE_VERSION'" >&2
  exit 1
fi

echo "Release contract verified: version $WORKSPACE_VERSION, build $APP_BUILD"
