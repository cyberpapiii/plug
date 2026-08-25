# Releasing plug

A release is a `v*` tag pushed to `main`. That tag triggers
`.github/workflows/release.yml`, which builds Linux standalone archives and the
universal macOS Plug.app, signs and notarizes its DMG, publishes a GitHub
release, and updates the Homebrew tap. macOS is distributed through Plug.app;
Darwin binaries ship only inside that app.

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

The public macOS artifact is a universal Plug.app inside a signed, notarized,
stapled DMG. The release workflow builds arm64 and x86_64 Darwin binaries only
as inputs to that app, then runs `scripts/sign-notarize-macos-app.sh`. It
hard-fails when any secret is missing rather than falling back to an unsigned
build, so a release cannot imply that an unsigned app is trusted.

The cargo-dist shell installer and Linux standalone archives are not macOS
installation paths. The published `plug-mcp-installer.sh` exits before network
or filesystem work on Darwin and directs users to the DMG or Homebrew Cask.

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

The Plug.app bundle and its embedded daemon are signed with the hardened
runtime and secure timestamp. The DMG is notarized and stapled, so Gatekeeper
can verify the downloaded app before first launch. The Linux archives and
Linux-only cargo-dist installer are separate artifacts; they do not provide a
macOS command-line installation path.

Homebrew installs the same signed DMG through the `plug-app` Cask. Plug.app
creates the command-line link and registers its background service on first
launch, so macOS installation requires a logged-in GUI session.

### Upgrading from an unsigned build

The Keychain grants access based on the code signature of the program asking.
Moving from a locally-signed or unsigned build to the Developer ID Plug.app is
a signature change, so macOS will prompt once more for access to Plug's stored
upstream credentials. Choosing Always Allow at that prompt holds for every
later Developer ID release.

## Local development signing is a different thing

`scripts/setup-codesigning.sh` creates a self-signed identity used by
`scripts/dev-reinstall.sh`. Its only job is to keep a locally-built binary's
signature stable across rebuilds so the Keychain stops re-prompting. It is not a
distribution identity and must never be used to sign a release.
