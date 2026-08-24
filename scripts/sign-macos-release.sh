#!/usr/bin/env bash
#
# Sign and notarize a release build of the `plug` binary with a Developer ID.
#
# This is the distribution signing path, distinct from the local development
# identity created by scripts/setup-codesigning.sh. A locally-signed binary only
# has to keep the Keychain quiet on one machine; a downloaded binary has to
# satisfy Gatekeeper on someone else's.
#
# Usage:
#   ./scripts/sign-macos-release.sh path/to/plug
#
# Required environment (all secrets, never committed):
#   MACOS_SIGNING_IDENTITY   "Developer ID Application: Name (TEAMID)"
#   MACOS_CERTIFICATE_P12    base64 of the exported Developer ID .p12
#   MACOS_CERTIFICATE_PASS   passphrase for that .p12
#   MACOS_NOTARY_KEY_P8      base64 of the App Store Connect API key .p8
#   MACOS_NOTARY_KEY_ID      the key's Key ID
#   MACOS_NOTARY_ISSUER_ID   the key's Issuer ID
#
# An App Store Connect API key is used rather than an Apple ID plus
# app-specific password because it is scoped to notarization and can be revoked
# without touching the account password.

set -euo pipefail

BINARY="${1:-}"

if [[ -z "$BINARY" ]]; then
  echo "usage: $0 <path-to-plug-binary>" >&2
  exit 2
fi

if [[ ! -f "$BINARY" ]]; then
  echo "error: $BINARY does not exist" >&2
  exit 2
fi

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: Developer ID signing requires macOS" >&2
  exit 2
fi

require_env() {
  local name="$1"
  if [[ -z "${!name:-}" ]]; then
    echo "error: $name is not set" >&2
    echo "       Release builds must be signed. See the header of this script" >&2
    echo "       for the full list of required secrets." >&2
    exit 2
  fi
}

for var in \
  MACOS_SIGNING_IDENTITY \
  MACOS_CERTIFICATE_P12 \
  MACOS_CERTIFICATE_PASS \
  MACOS_NOTARY_KEY_P8 \
  MACOS_NOTARY_KEY_ID \
  MACOS_NOTARY_ISSUER_ID; do
  require_env "$var"
done

WORK_DIR="$(mktemp -d)"
KEYCHAIN="$WORK_DIR/build.keychain-db"
# Ephemeral keychain password: the keychain is deleted on exit, so this value
# only has to be unguessable for the length of the build.
KEYCHAIN_PASS="$(head -c 32 /dev/urandom | base64)"

ORIGINAL_KEYCHAINS=""

cleanup() {
  # Restore the search list first: leaving a deleted keychain in it breaks
  # later `security` calls. A CI runner is thrown away, but this script can
  # also be run on a real machine.
  if [[ -n "$ORIGINAL_KEYCHAINS" ]]; then
    # shellcheck disable=SC2086
    security list-keychains -d user -s $ORIGINAL_KEYCHAINS 2>/dev/null || true
  fi
  security delete-keychain "$KEYCHAIN" 2>/dev/null || true
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT

echo "==> Creating an ephemeral signing keychain"
security create-keychain -p "$KEYCHAIN_PASS" "$KEYCHAIN"
security set-keychain-settings -lut 900 "$KEYCHAIN"
security unlock-keychain -p "$KEYCHAIN_PASS" "$KEYCHAIN"

echo "==> Importing the Developer ID certificate"
printf '%s' "$MACOS_CERTIFICATE_P12" | base64 --decode > "$WORK_DIR/certificate.p12"
security import "$WORK_DIR/certificate.p12" \
  -k "$KEYCHAIN" \
  -P "$MACOS_CERTIFICATE_PASS" \
  -T /usr/bin/codesign
rm -f "$WORK_DIR/certificate.p12"

# Without this, codesign blocks on a GUI prompt that CI can never answer.
security set-key-partition-list \
  -S apple-tool:,apple:,codesign: \
  -s -k "$KEYCHAIN_PASS" \
  "$KEYCHAIN" >/dev/null

# Put the build keychain first so codesign resolves the identity from it, and
# keep the existing search list so system roots still validate the chain.
ORIGINAL_KEYCHAINS="$(security list-keychains -d user | tr -d '"' | xargs)"
# shellcheck disable=SC2086
security list-keychains -d user -s "$KEYCHAIN" $ORIGINAL_KEYCHAINS


if ! security find-identity -v -p codesigning "$KEYCHAIN" \
  | grep -qF "$MACOS_SIGNING_IDENTITY"; then
  echo "error: '$MACOS_SIGNING_IDENTITY' is not a valid codesigning identity" >&2
  echo "       in the imported certificate. Check that the secret holds a" >&2
  echo "       Developer ID Application certificate and that the identity" >&2
  echo "       string matches it exactly." >&2
  exit 1
fi

echo "==> Signing $BINARY"
# --options runtime enables the hardened runtime, which notarization requires.
# --timestamp binds a secure timestamp so the signature outlives the
# certificate's expiry.
codesign \
  --force \
  --sign "$MACOS_SIGNING_IDENTITY" \
  --keychain "$KEYCHAIN" \
  --options runtime \
  --timestamp \
  "$BINARY"

codesign --verify --strict --verbose=2 "$BINARY"

echo "==> Submitting for notarization"
printf '%s' "$MACOS_NOTARY_KEY_P8" | base64 --decode > "$WORK_DIR/notary.p8"
chmod 600 "$WORK_DIR/notary.p8"

# notarytool only accepts an archive, never a bare executable.
ZIP="$WORK_DIR/plug-notarize.zip"
ditto -c -k --keepParent "$BINARY" "$ZIP"

xcrun notarytool submit "$ZIP" \
  --key "$WORK_DIR/notary.p8" \
  --key-id "$MACOS_NOTARY_KEY_ID" \
  --issuer "$MACOS_NOTARY_ISSUER_ID" \
  --wait \
  --timeout 30m

rm -f "$WORK_DIR/notary.p8"

# A stapled ticket can only be attached to a bundle, disk image, or installer
# package, so a bare CLI binary cannot carry one. Gatekeeper resolves the
# notarization online instead, which is why the signature itself must verify
# against the Developer ID chain below.
echo "==> Verifying the signature against the Developer ID policy"
codesign --verify --strict --verbose=4 "$BINARY"
codesign --display --verbose=4 "$BINARY" 2>&1 | grep -E 'Authority|Timestamp|Runtime'

echo
echo "Signed and notarized: $BINARY"
