#!/usr/bin/env bash
#
# setup-codesigning.sh — one-time, per-machine macOS code-signing setup for plug.
#
# Why: plug stores upstream OAuth credentials in the macOS login Keychain. A
# locally-built Rust binary is ad-hoc signed, and its signature changes on every
# `cargo install`, so the Keychain "Always Allow" ACL never persists and macOS
# re-prompts constantly. Signing the binary with a STABLE self-signed identity
# binds the ACL to the certificate (not the per-build hash), so the approval
# sticks across rebuilds.
#
# This script is idempotent and safe to re-run:
#   - no-ops on non-macOS
#   - skips creation if a valid "Plug Local Signing" identity already exists
#   - signs ~/.cargo/bin/plug if it is present
#
# The trust step (security add-trusted-cert) shows a GUI dialog asking for your
# login password — that is expected and required to mark the cert trusted.
#
# See: docs/solutions/integration-issues/local-codesigning-identity-stops-keychain-reprompts.md

set -euo pipefail

IDENTITY="Plug Local Signing"
SIGNING_DIR="${PLUG_SIGNING_DIR:-$HOME/.config/plug-signing}"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
CARGO_PLUG="${CARGO_HOME:-$HOME/.cargo}/bin/plug"
P12_PASS="pluglocal" # local-only passphrase for the on-disk p12 (chmod 600)

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "Not macOS — code-signing setup is not needed on this platform. Skipping."
  exit 0
fi

identity_is_valid() {
  security find-identity -v -p codesigning 2>/dev/null | grep -qF "$IDENTITY"
}

sign_binary() {
  if [[ -e "$CARGO_PLUG" ]]; then
    echo "==> Signing $CARGO_PLUG with '$IDENTITY'"
    codesign --force -s "$IDENTITY" "$CARGO_PLUG"
    codesign -dv --verbose=2 "$CARGO_PLUG" 2>&1 | grep -E 'Authority|Signature' || true
  else
    echo "Note: $CARGO_PLUG not found — build/install first (e.g. ./scripts/dev-reinstall.sh), then it will be signed."
  fi
}

if identity_is_valid; then
  echo "✓ '$IDENTITY' is already a valid code-signing identity. Nothing to create."
  sign_binary
  echo
  echo "Done. Re-sign on every rebuild with ./scripts/dev-reinstall.sh (it signs automatically)."
  exit 0
fi

echo "==> Creating a stable self-signed code-signing identity: '$IDENTITY'"
mkdir -p "$SIGNING_DIR"
chmod 700 "$SIGNING_DIR"

# 1. Self-signed cert. BOTH basic Key Usage (digitalSignature) and Extended Key
#    Usage (codeSigning) are required by macOS's code-signing policy. Omitting
#    KU yields "Invalid Key Usage for policy"; omitting EKU yields no identity.
openssl req -x509 -newkey rsa:2048 \
  -keyout "$SIGNING_DIR/key.pem" -out "$SIGNING_DIR/cert.pem" \
  -days 3650 -nodes \
  -subj "/CN=$IDENTITY" \
  -addext "keyUsage=critical,digitalSignature" \
  -addext "extendedKeyUsage=critical,codeSigning" \
  -addext "basicConstraints=critical,CA:false" >/dev/null 2>&1

# 2. Bundle into PKCS#12 with LEGACY algorithms (OpenSSL 3 defaults fail macOS
#    `security import` with "MAC verification failed"). Real passphrase required.
openssl pkcs12 -export \
  -inkey "$SIGNING_DIR/key.pem" -in "$SIGNING_DIR/cert.pem" \
  -out "$SIGNING_DIR/plug-signing.p12" \
  -name "$IDENTITY" -passout "pass:$P12_PASS" -legacy >/dev/null 2>&1

chmod 600 "$SIGNING_DIR/key.pem" "$SIGNING_DIR/plug-signing.p12"

# 3. Import cert+key, granting codesign access to the key.
echo "==> Importing identity into the login keychain"
security import "$SIGNING_DIR/plug-signing.p12" \
  -k "$KEYCHAIN" -P "$P12_PASS" -T /usr/bin/codesign

# 4. Trust the cert FOR CODE SIGNING ONLY. This pops a GUI dialog for your login
#    password — that is expected.
echo "==> Trusting the cert for code signing (a login-password dialog is expected)"
security add-trusted-cert -r trustRoot -p codeSign "$SIGNING_DIR/cert.pem"

# 5. Verify.
echo "==> Verifying"
if ! identity_is_valid; then
  echo "error: '$IDENTITY' did not become a valid code-signing identity." >&2
  echo "       Check: security find-identity -v -p codesigning" >&2
  exit 1
fi
echo "✓ '$IDENTITY' is now a valid code-signing identity."

sign_binary

cat <<EOF

Setup complete. Next:
  - Restart plug in your LOGIN SESSION: plug stop && plug start
  - You will get ONE more round of "Always Allow" Keychain prompts (one per OAuth
    upstream) because the binary identity just changed. Click "Always Allow" —
    those approvals now bind to the stable "$IDENTITY" identity and won't recur.
  - From now on, rebuild with ./scripts/dev-reinstall.sh — it re-signs automatically.
EOF
