---
title: A stable self-signed code-signing identity stops repeated macOS Keychain prompts
date: 2026-06-11
category: integration-issues
module: plug install / codesign / macos keychain
problem_type: integration_issue
component: tooling
symptoms:
  - "macOS shows 'plug wants to use your confidential information stored in plug in your keychain' on every launch; clicking 'Always Allow' never makes it stick"
  - "A burst of keychain authorization prompts each time an agent or app connected to plug starts"
  - "codesign -dv reports Signature=adhoc and flags=0x20002(adhoc,linker-signed)"
root_cause: incomplete_setup
resolution_type: environment_setup
severity: medium
tags:
  - keychain
  - codesign
  - code-signing
  - macos
  - oauth
  - self-signed-cert
  - developer-experience
---

# A stable self-signed code-signing identity stops repeated macOS Keychain prompts

## Problem

On macOS, plug re-prompts for Keychain access ("plug wants to use your
confidential information stored in 'plug' in your keychain") constantly — every
time an agent or app that talks to plug starts. Clicking **Always Allow** does
not stop it: the prompt returns after the next rebuild and across many
short-lived processes. Frustrating, and easy to mistake for a broken install.

## Symptoms

- The Keychain authorization dialog appears repeatedly, often several at once.
- **Always Allow** appears not to persist — the same upstreams ask again later.
- `codesign -dv --verbose=2 ~/.cargo/bin/plug` shows `Signature=adhoc` and
  `flags=0x20002(adhoc,linker-signed)`.
- `security find-identity -v -p codesigning` lists **0 valid identities** (no
  stable signer is in play).

## What Didn't Work

- **Clicking "Always Allow."** It works for that exact binary, but the ACL is
  bound to the binary's *code signature*. The next `cargo install` produces a
  new ad-hoc signature (a fresh CDHash), so macOS treats it as a brand-new
  program and the approval no longer matches.
- **Exporting the cert to PKCS#12 with OpenSSL 3 defaults.** `security import`
  rejected it with `MAC verification failed during PKCS12 import (wrong
  password?)`. OpenSSL 3 writes the p12 MAC with an algorithm Apple's `security`
  tool can't verify; an empty passphrase makes the mismatch worse. Fixed by
  exporting with `-legacy` and a real passphrase.
- **Signing with a self-signed cert that had only Extended Key Usage.**
  `codesign -s "Plug Local Signing"` returned `no identity found`, and
  `find-identity -p codesigning` showed the cert as `CSSMERR_TP_NOT_TRUSTED`.
  codesign only signs with an identity macOS considers *valid* — an untrusted
  self-signed cert is not valid until explicitly trusted for the codeSign policy.
- **Trusting the cert, but with no basic Key Usage extension.** After
  `add-trusted-cert`, `find-identity -v` then reported
  `Plug Local Signing (Invalid Key Usage for policy)`. Apple's code-signing
  policy requires the leaf cert to carry `Key Usage: Digital Signature`; the
  first cert only set *Extended* Key Usage (codeSigning), not basic Key Usage.

## Solution

Give the binary a **stable, trusted self-signed code-signing identity** and sign
with it after every build. Because the Keychain ACL binds to the signature's
*designated requirement* (which references the cert, not the per-build CDHash),
"Always Allow" then persists across rebuilds.

One-time setup (now scripted as `scripts/setup-codesigning.sh`):

```sh
# 1. Self-signed cert — BOTH Key Usage (digitalSignature) AND Extended Key
#    Usage (codeSigning) are required by macOS's code-signing policy.
openssl req -x509 -newkey rsa:2048 -keyout key.pem -out cert.pem -days 3650 -nodes \
  -subj "/CN=Plug Local Signing" \
  -addext "keyUsage=critical,digitalSignature" \
  -addext "extendedKeyUsage=critical,codeSigning" \
  -addext "basicConstraints=critical,CA:false"

# 2. Bundle into a PKCS#12 with LEGACY algorithms macOS can import (and a real password).
openssl pkcs12 -export -inkey key.pem -in cert.pem -out plug-signing.p12 \
  -name "Plug Local Signing" -passout pass:pluglocal -legacy

# 3. Import cert+key, letting codesign use the key.
security import plug-signing.p12 \
  -k ~/Library/Keychains/login.keychain-db -P pluglocal -T /usr/bin/codesign

# 4. Trust it FOR CODE SIGNING ONLY (prompts for login password — expected).
security add-trusted-cert -r trustRoot -p codeSign cert.pem

# 5. Confirm it is now valid (expect "1 valid identities found", no error suffix).
security find-identity -v -p codesigning
```

Then sign on every install (now wired into `scripts/dev-reinstall.sh`):

```sh
cargo install --path plug --force
codesign --force -s "Plug Local Signing" ~/.cargo/bin/plug
codesign -dv --verbose=2 ~/.cargo/bin/plug 2>&1 | grep Authority
# → Authority=Plug Local Signing   (no longer Signature=adhoc)
```

After the first signed restart, macOS prompts **once more** per OAuth upstream
(the identity just changed from ad-hoc to the stable cert). Click **Always
Allow** — those approvals now bind to the stable identity and do not return on
future rebuilds.

## Why This Works

A macOS Keychain "Always Allow" ACL entry is keyed to the requesting
executable's **code signature**, specifically its *designated requirement* (DR).

- An **ad-hoc** signature has no signer identity — its DR is essentially the
  CDHash (a hash of the exact bytes). Every rebuild changes the bytes, changes
  the CDHash, changes the DR, and invalidates the ACL match → re-prompt.
- A signature from a **stable cert** produces a DR that references the
  certificate, which does not change between rebuilds. The ACL match holds, so
  the approval persists.

The two failed cert attempts map directly to macOS's validity gate for signing
identities: the cert must be **trusted** for the `codeSign` policy
(`add-trusted-cert -p codeSign`) **and** carry the **basic Key Usage =
Digital Signature** extension (not just Extended Key Usage). Miss either and
`codesign` reports "no identity found" or `find-identity -v` flags it
`Invalid Key Usage for policy`.

## Prevention

- **Always re-sign after installing.** Use `scripts/dev-reinstall.sh` (which
  signs automatically when the identity exists) instead of a bare
  `cargo install`. A bare install drops back to ad-hoc and the prompts return.
- **Run `scripts/setup-codesigning.sh` once per machine** after cloning. It is
  idempotent: it no-ops on non-macOS, and skips creation if a valid
  `Plug Local Signing` identity already exists.
- **When generating a code-signing cert by hand, set both KU and EKU**:
  `keyUsage=critical,digitalSignature` and
  `extendedKeyUsage=critical,codeSigning`. KU-only or EKU-only certs fail the
  policy.
- **Export p12s for macOS with `-legacy`** (OpenSSL 3+) and a non-empty
  passphrase, or `security import` fails MAC verification.
- This is not plug-specific: **any locally-built, unsigned/ad-hoc tool that
  reads the macOS Keychain re-prompts on every rebuild.** The same
  self-signed-identity fix applies broadly (it was already visible here as the
  untrusted `iMessage Max Dev` cert in the same keychain).

## Related Issues

- `docs/solutions/integration-issues/daemon-restart-context-keychain-and-spawn-storm.md`
  — **related but distinct.** That doc is about the daemon *hanging on a Keychain
  read* when started outside the login session (an operability/launch-context
  failure); this doc is about *repeated Keychain prompts* from an unstable
  signature. They are the two halves of "the macOS Keychain ACL is keyed to the
  plug binary's signing identity." A stable signature reduces re-approval
  friction but does **not** remove that doc's login-session requirement — a
  detached/sandboxed process still blocks on the Keychain read regardless of how
  the binary is signed.
- `docs/solutions/integration-issues/2026-03-18-oauth-credential-snapshot-unification.md`
  — context on what credentials live in the Keychain that these prompts protect.
