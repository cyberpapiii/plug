#!/usr/bin/env bash
# Build Plug.app from this working tree and install it on this Mac.
#
#   scripts/dev-install.sh
#
# This is the inner loop: edit, run this, try it. About a minute or two, no
# network, no notarization, no version bump, no commit. The app is signed with
# the same Developer ID as a release, so the app's own signature check, the
# CLI's check, and the Keychain "Always Allow" entries all keep working. The
# build number is the current time, which is always newer than the installed
# build, so the app sees its daemon as stale and replaces it itself. It is also
# far above any release build number, so Sparkle never offers a downgrade.
#
# Releases are a different loop: scripts/release.sh, when you decide.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

case "${1:-}" in
  -h | --help)
    sed -n '2,14p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
esac

# shellcheck source=scripts/lib/install-app.sh
source "$repo_root/scripts/lib/install-app.sh"

# The Developer ID certificate with the latest expiry. Naming it by title is
# ambiguous once a renewed certificate sits beside the old one.
pick_identity() {
  local best_hash="" best_date=0 hash pem date
  while read -r hash; do
    pem="$(security find-certificate -a -c 'Developer ID Application' -Z -p |
      awk -v H="$hash" '/SHA-1 hash/{hit=($3==H)} hit&&/BEGIN CERT/{p=1} p{print} hit&&/END CERT/{exit}')"
    date="$(printf '%s\n' "$pem" | openssl x509 -noout -enddate | sed 's/notAfter=//')"
    date="$(date -j -f '%b %e %T %Y %Z' "$date" +%s 2>/dev/null || echo 0)"
    if (( date > best_date )); then best_date=$date; best_hash=$hash; fi
  done < <(security find-identity -v -p codesigning | awk '/Developer ID Application/{print $2}')
  [[ -n "$best_hash" ]] || { echo "dev-install: no Developer ID Application identity in the keychain" >&2; return 1; }
  printf '%s' "$best_hash"
}
identity="${PLUG_SIGNING_IDENTITY:-$(pick_identity)}"

command -v xcodegen >/dev/null || { echo "dev-install: brew install xcodegen" >&2; exit 1; }

version="$(cargo metadata --no-deps --format-version 1 |
  python3 -c 'import json,sys; d=json.load(sys.stdin); print(next(p["version"] for p in d["packages"] if p["name"] == "plug-mcp"))')"
build="$(date +%s)"
derived="PlugApp/.build"
app="$derived/Build/Products/Release/Plug.app"

echo "dev-install: building Plug.app $version (build $build)"
xcodegen generate --spec PlugApp/project.yml --quiet
log="$(mktemp -t plug-dev-install)"
if ! xcodebuild build \
  -project PlugApp/PlugApp.xcodeproj \
  -scheme PlugApp \
  -configuration Release \
  -destination 'platform=macOS' \
  -derivedDataPath "$derived" \
  -quiet \
  CODE_SIGNING_ALLOWED=NO \
  MARKETING_VERSION="$version" \
  CURRENT_PROJECT_VERSION="$build" >"$log" 2>&1; then
  grep -E 'error:|error\[|\*\* BUILD FAILED' "$log" | head -40 >&2
  echo "dev-install: build failed; full log: $log" >&2
  exit 1
fi
rm -f "$log"

echo "dev-install: signing"
# Inside-out, the same order as the release signer. --deep is not used for
# signing on purpose: it misses the daemon in Resources and misapplies
# entitlements to Sparkle's helpers.
sign() { codesign --force --sign "$identity" --options runtime "$@"; }
sparkle="$app/Contents/Frameworks/Sparkle.framework/Versions/Current"
sign "$sparkle/XPCServices/Installer.xpc"
sign --preserve-metadata=entitlements "$sparkle/XPCServices/Downloader.xpc"
sign "$sparkle/Autoupdate"
sign "$sparkle/Updater.app"
sign "$app/Contents/Frameworks/Sparkle.framework"
sign --identifier com.cyberpapiii.plug.daemon "$app/Contents/Resources/plug"
sign "$app"
codesign --verify --deep --strict "$app"
# The exact requirement the app and the CLI check at launch.
codesign --verify \
  '-R=anchor apple generic and identifier "com.cyberpapiii.plug" and certificate leaf[subject.OU] = "HJF7LN64XX"' \
  "$app"

install_app_bundle "$app" "$version"
