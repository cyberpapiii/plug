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

# shellcheck source=scripts/lib/install-app.sh
source "$repo_root/scripts/lib/install-app.sh"

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

# Check only the DMG line. The checksums file also covers the appcast and Cask,
# which were not downloaded, and `shasum -c` fails on a missing file.
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

# The bundle stays mounted through the copy; the shared installer copies it
# out before it touches /Applications.
install_app_bundle "$staged_app" "$version"
