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

**Easiest path (any install) — the built-in command:**

```sh
plug codesign-setup   # creates the identity if missing, then signs the running binary
```

`plug codesign-setup` is install-path-agnostic (cargo, Homebrew, release download)
and self-contained — it runs all the steps below for you. `plug doctor` surfaces
the need for it: a `codesign_identity` check warns when plug is ad-hoc signed and
keychain-backed OAuth upstreams are configured, and points at the command.
`scripts/setup-codesigning.sh` is the equivalent standalone shell script for the
repo clone flow.

The underlying one-time steps (what the command/script automate):

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

Then use the staged installer for every local rebuild:

```sh
./scripts/dev-reinstall.sh --quick
codesign --verify --deep --strict ~/.cargo/bin/plug
codesign -dv --verbose=2 ~/.cargo/bin/plug 2>&1 | grep Authority
# → Authority=Plug Local Signing   (no longer Signature=adhoc)
```

The script installs into a private same-filesystem staging directory, signs and
verifies the candidate, smoke-tests it, and only then atomically renames it over
the live binary. Do not replace the live binary first and sign it afterward:
an auto-respawning client could execute that unsigned intermediate build and
reopen the Keychain authorization flood.

Daemon operator queries must also stay off the synchronous Keychain path.
`AuthStatus` reads the in-memory credential cache and protected file mirror
only; a keyring-only credential is reported as unavailable at runtime and can
still be recovered by the explicit credential-loading paths. This keeps a
read-only status request from parking Tokio's I/O driver in
`SecKeychainFindGenericPassword` and freezing both IPC and HTTP.

After the first signed restart, macOS prompts **once more** per OAuth upstream
(the identity just changed from ad-hoc to the stable cert). Click **Always
Allow** — those approvals now bind to the stable identity and do not return on
future rebuilds.

### July 2026 follow-up: signing was necessary but not sufficient

A later 50-plus-dialog burst exposed two independent amplifiers that stable
signing could not solve by itself:

- OAuth tests were using the production token directory and login Keychain.
  Each short-lived test binary could therefore request real credential access.
- Daemon contenders acquired the singleton lock only after starting upstreams.
  Several simultaneous clients could all reach Keychain before one process won.

Plug now isolates test credentials, takes the daemon lock before upstream
initialization, makes losing auto-start attempts follow the winning daemon, and
uses the protected token-file mirror before probing Keychain during normal
startup. Stable signing still matters for genuine Keychain recovery and
diagnostic access, but it is no longer asked to paper over process and test
isolation bugs.

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

- **`plug doctor` catches it.** The `codesign_identity` check warns when plug is
  ad-hoc signed and OAuth upstreams are configured, so the condition is
  discoverable on any install path (not just the repo clone).
- **`plug codesign-setup` fixes it** in one command, idempotently — it creates the
  identity if missing and signs the running binary, regardless of install method.
- **Always re-sign after installing.** Use `scripts/dev-reinstall.sh` (which
  signs automatically when the identity exists) instead of a bare
  `cargo install`. A bare install drops back to ad-hoc and the prompts return.
- **Never let tests use production credential paths.** Workspace OAuth tests
  install an isolated temporary token directory and in-memory keyring before
  the first credential operation; keep regression coverage for that boundary.
- **Acquire singleton ownership before external initialization.** A daemon that
  cannot own the runtime lock must not start upstreams or read credentials.
- **Run `plug codesign-setup` (or `scripts/setup-codesigning.sh`) once per
  machine.** Both are idempotent: they no-op on non-macOS and skip creation when
  a valid `Plug Local Signing` identity already exists.
- **Distributed binaries need a real fix:** Developer ID signing + notarization in
  the release pipeline so release/Homebrew installs are signed out of the box —
  tracked in `todos/069-pending-p3-release-binary-codesigning-notarization.md`.
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
