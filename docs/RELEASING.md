# Releasing plug

A release is a `v*` tag pushed to `main`. That tag triggers
`.github/workflows/release.yml`, which builds every target, signs and notarizes
the macOS binaries, publishes a GitHub release, and updates the Homebrew tap.

## Before tagging

1. `main` is green: `cargo test --workspace`, `cargo clippy --workspace
   --all-targets -- -D warnings`, `cargo fmt --check`, `cargo deny check`.
2. `CHANGELOG.md` covers everything since the previous tag. Release notes are
   generated from the commit history by `git-cliff`, so the commit subjects are
   the release notes; fix them before tagging, not after.
3. `docs/PROJECT-STATE-SNAPSHOT.md` matches `main`.
4. `workspace.package.version` in the root `Cargo.toml` matches the tag you are
   about to push, and `Cargo.lock` has been regenerated to match.

## Tagging

```bash
version=0.5.0
git tag -a "v$version" -m "plug $version" && git push origin "v$version"
```

The workflow does the rest. Watch it: a failed signing step stops the release
before anything is published, which is the intended behaviour.

## macOS code signing

Downloaded macOS binaries are signed with a Developer ID Application
certificate and notarized by Apple. Without that, Gatekeeper refuses to run
anything a browser downloaded, and users have to strip the quarantine attribute
by hand — an instruction no one should have to follow to install a tool that
holds their credentials.

Signing runs in `scripts/sign-macos-release.sh`, which the release workflow
calls on both `apple-darwin` targets. The script hard-fails when any secret is
missing rather than falling back to an unsigned build, so a release cannot ship
an unsigned binary while implying otherwise.

### Required repository secrets

| Secret | What it holds |
| --- | --- |
| `MACOS_SIGNING_IDENTITY` | The full identity string, `Developer ID Application: Name (TEAMID)` |
| `MACOS_CERTIFICATE_P12` | Base64 of the exported Developer ID Application certificate and private key |
| `MACOS_CERTIFICATE_PASS` | Passphrase set when exporting that `.p12` |
| `MACOS_NOTARY_KEY_P8` | Base64 of the App Store Connect API key `.p8` |
| `MACOS_NOTARY_KEY_ID` | The API key's Key ID |
| `MACOS_NOTARY_ISSUER_ID` | The API key's Issuer ID |
| `SPARKLE_PRIVATE_KEY` | Sparkle EdDSA private key exported by `generate_keys -x` |
| `HOMEBREW_TAP_TOKEN` | Token allowed to update `cyberpapiii/homebrew-tap` |

Export the certificate from Keychain Access as a `.p12`, then
`base64 -i certificate.p12 | pbcopy`. Create the notarization key in App Store
Connect under Users and Access, Integrations, with the Developer role; the `.p8`
downloads once and cannot be retrieved again.

An App Store Connect API key is used instead of an Apple ID plus app-specific
password because it is scoped to notarization and can be revoked on its own.

### What is signed, and what that does not cover

The `plug` executable is signed with the hardened runtime and a secure
timestamp, then notarized. A notarization ticket can only be stapled to a
bundle, disk image, or installer package, so a bare command-line binary cannot
carry one; Gatekeeper resolves the notarization online instead. That is normal
for CLI tools, but it means a first launch on a machine with no network may be
slower or, on a quarantined copy, blocked until the check completes.

Homebrew and the shell installer do not set the quarantine attribute, so those
paths were never blocked. Signing still matters for them: it is what makes the
binary's origin verifiable, and it is what keeps the macOS Keychain from
re-prompting after every upgrade.

### Upgrading from an unsigned build

The Keychain grants access based on the code signature of the program asking.
Moving from a locally-signed or unsigned build to the Developer ID identity is a
signature change, so macOS will prompt once more for access to plug's stored
upstream credentials. Choosing Always Allow at that prompt holds for every
later Developer ID release.

## Local development signing is a different thing

`scripts/setup-codesigning.sh` creates a self-signed identity used by
`scripts/dev-reinstall.sh`. Its only job is to keep a locally-built binary's
signature stable across rebuilds so the Keychain stops re-prompting. It is not a
distribution identity and must never be used to sign a release.
