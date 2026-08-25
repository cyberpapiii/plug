#!/usr/bin/env bash
# Sign, notarize, staple, and verify Plug.app and its distribution DMG.
set -euo pipefail

APP="${1:-}"
DMG="${2:-}"
if [[ ! -d "$APP" || -z "$DMG" ]]; then
  echo "usage: $0 path/to/Plug.app path/to/Plug.dmg" >&2
  exit 2
fi
if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: macOS is required" >&2
  exit 2
fi

require_env() {
  [[ -n "${!1:-}" ]] || { echo "error: $1 is not set" >&2; exit 2; }
}
for var in MACOS_SIGNING_IDENTITY MACOS_CERTIFICATE_P12 MACOS_CERTIFICATE_PASS \
  MACOS_NOTARY_KEY_P8 MACOS_NOTARY_KEY_ID MACOS_NOTARY_ISSUER_ID; do
  require_env "$var"
done

WORK_DIR="$(mktemp -d)"
KEYCHAIN="$WORK_DIR/build.keychain-db"
KEYCHAIN_PASS="$(head -c 32 /dev/urandom | base64)"
ORIGINAL_KEYCHAINS="$(security list-keychains -d user | tr -d '"' | xargs)"
cleanup() {
  # shellcheck disable=SC2086
  security list-keychains -d user -s $ORIGINAL_KEYCHAINS 2>/dev/null || true
  security delete-keychain "$KEYCHAIN" 2>/dev/null || true
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT

security create-keychain -p "$KEYCHAIN_PASS" "$KEYCHAIN"
security set-keychain-settings -lut 900 "$KEYCHAIN"
security unlock-keychain -p "$KEYCHAIN_PASS" "$KEYCHAIN"
printf '%s' "$MACOS_CERTIFICATE_P12" | base64 --decode > "$WORK_DIR/certificate.p12"
security import "$WORK_DIR/certificate.p12" -k "$KEYCHAIN" -P "$MACOS_CERTIFICATE_PASS" -T /usr/bin/codesign
: > "$WORK_DIR/certificate.p12"
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$KEYCHAIN_PASS" "$KEYCHAIN" >/dev/null
# shellcheck disable=SC2086
security list-keychains -d user -s "$KEYCHAIN" $ORIGINAL_KEYCHAINS

printf '%s' "$MACOS_NOTARY_KEY_P8" | base64 --decode > "$WORK_DIR/notary.p8"
chmod 600 "$WORK_DIR/notary.p8"

echo "==> Signing app"
codesign --force --deep --sign "$MACOS_SIGNING_IDENTITY" --keychain "$KEYCHAIN" \
  --options runtime --timestamp "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"

echo "==> Notarizing app"
ditto -c -k --keepParent "$APP" "$WORK_DIR/Plug.zip"
xcrun notarytool submit "$WORK_DIR/Plug.zip" --key "$WORK_DIR/notary.p8" \
  --key-id "$MACOS_NOTARY_KEY_ID" --issuer "$MACOS_NOTARY_ISSUER_ID" --wait --timeout 30m
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"
spctl --assess --type exec --verbose=2 "$APP"

echo "==> Creating DMG"
mkdir -p "$WORK_DIR/dmg"
ditto "$APP" "$WORK_DIR/dmg/Plug.app"
ln -s /Applications "$WORK_DIR/dmg/Applications"
hdiutil create -volname Plug -srcfolder "$WORK_DIR/dmg" -ov -format UDZO "$DMG"
codesign --force --sign "$MACOS_SIGNING_IDENTITY" --keychain "$KEYCHAIN" --timestamp "$DMG"

echo "==> Notarizing DMG"
xcrun notarytool submit "$DMG" --key "$WORK_DIR/notary.p8" \
  --key-id "$MACOS_NOTARY_KEY_ID" --issuer "$MACOS_NOTARY_ISSUER_ID" --wait --timeout 30m
xcrun stapler staple "$DMG"
xcrun stapler validate "$DMG"
spctl --assess --type open --context context:primary-signature --verbose=2 "$DMG"

: > "$WORK_DIR/notary.p8"
echo "Signed, notarized, and stapled: $DMG"
