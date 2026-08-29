#!/usr/bin/env bash
# Install a published Plug.app release onto this Mac, unattended.
#
#   scripts/install-release.sh            install the latest published release
#   scripts/install-release.sh 0.8.3      install a specific version
#
# It downloads the release DMG, checks it against the release checksums, refuses
# anything Gatekeeper does not accept, replaces /Applications/Plug.app, and waits
# for the daemon to answer before it reports success. Nothing here needs a click.
#
# Replacing the bundle is the one destructive step, so it happens only after the
# download is verified, and only once the running app has quit on its own. The
# swap moves the old bundle aside rather than deleting it, because every MCP
# client's `plug connect` re-execs the bundle's binary and macOS aborts a running
# process whose signed executable is unlinked underneath it.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

case "${1:-}" in
  -h | --help)
    sed -n '2,12p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
esac

version="${1:-}"
if [[ -z "$version" ]]; then
  version="$(gh release view --json tagName --jq .tagName)"
fi
version="${version#v}"
tag="v$version"

app="/Applications/Plug.app"
# Retired bundles wait here for their last clients to exit. Outside /Applications
# so a superseded copy never shows up in Spotlight or Launchpad.
attic="${TMPDIR:-/tmp}/plug-retired-bundles"
staging="$(mktemp -d)"
mount_point=""
cleanup() {
  [[ -z "$mount_point" ]] || hdiutil detach "$mount_point" -quiet 2>/dev/null || true
  rm -rf "$staging"
}
trap cleanup EXIT

echo "install: fetching $tag"
gh release download "$tag" --dir "$staging" --pattern "Plug-$version.dmg" --pattern checksums.sha256

dmg="$staging/Plug-$version.dmg"
[[ -f "$dmg" ]] || { echo "install: $tag has no Plug-$version.dmg" >&2; exit 1; }

# Check only the DMG line. The checksums file also covers Linux tarballs that
# were never downloaded, and `shasum -c` fails on a missing file.
( cd "$staging" && grep " Plug-$version.dmg\$" checksums.sha256 | shasum -a 256 -c - )

mount_point="$(mktemp -d)"
hdiutil attach "$dmg" -mountpoint "$mount_point" -nobrowse -readonly -quiet
staged_app="$mount_point/Plug.app"
[[ -d "$staged_app" ]] || { echo "install: no Plug.app inside $dmg" >&2; exit 1; }

# Gatekeeper is the gate that matters: it fails on an unsigned, re-signed, or
# un-notarized bundle, which is exactly what must never reach /Applications.
spctl --assess --type execute "$staged_app"

staged_version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$staged_app/Contents/Info.plist")"
[[ "$staged_version" == "$version" ]] ||
  { echo "install: DMG contains $staged_version, expected $version" >&2; exit 1; }

if pgrep -qf "$app/Contents/MacOS/Plug"; then
  echo "install: quitting the running Plug.app"
  osascript -e 'tell application "Plug" to quit' >/dev/null 2>&1 || true
  for _ in $(seq 1 30); do
    pgrep -qf "$app/Contents/MacOS/Plug" || break
    sleep 1
  done
  pgrep -qf "$app/Contents/MacOS/Plug" &&
    { echo "install: Plug.app is still running; not replacing it" >&2; exit 1; }
fi

# A hand-installed LaunchAgent for the daemon predates the app-owned era and
# would keep launchd pointed at whatever binary it names. The app registers the
# daemon through SMAppService, so leaving one behind means two owners.
legacy_plist="$HOME/Library/LaunchAgents/com.plug.daemon.plist"
if [[ -f "$legacy_plist" ]]; then
  echo "install: removing the CLI-owned LaunchAgent"
  launchctl bootout "gui/$(id -u)/com.plug.daemon" 2>/dev/null || true
  rm -f "$legacy_plist"
fi

echo "install: replacing $app"
# Stage the new bundle beside the old one, then swap by rename. `rm -rf` here
# would kill every `plug connect` currently running the old bundle's binary:
# they die with SIGABRT the moment their signed executable is unlinked. A rename
# keeps that inode reachable, so those clients live until they exit on their own.
incoming="/Applications/.Plug.app.incoming.$$"
retired="$attic/Plug.app.$(date +%Y%m%d-%H%M%S)"
rm -rf "$incoming"
ditto "$staged_app" "$incoming"
if [[ -d "$app" ]]; then
  mkdir -p "$attic"
  mv "$app" "$retired"
fi
mv "$incoming" "$app"
hdiutil detach "$mount_point" -quiet
mount_point=""

open -a "$app"

# Yesterday's retired bundles have no processes left to protect.
find "$attic" -maxdepth 1 -name 'Plug.app.*' -mtime +0 -exec rm -rf {} + 2>/dev/null || true

echo "install: waiting for the daemon"
socket="$HOME/Library/Application Support/plug/plug.sock"
for _ in $(seq 1 120); do
  if [[ -S "$socket" ]] && "$app/Contents/Resources/plug" status >/dev/null 2>&1; then
    echo "install: Plug $version is running"
    exit 0
  fi
  sleep 1
done

echo "install: Plug $version is installed, but the daemon did not answer within 120s." >&2
echo "install: check $HOME/Library/Logs/plug" >&2
exit 1
