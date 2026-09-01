# Plug 0.7.0

Plug 0.7.0 makes macOS one product with one owner. Install Plug.app once and
the app owns the menu-bar experience, `plug` command, background daemon, MCP
client links, and updates. Linux remains a standalone CLI platform, while
source development uses an explicitly isolated command.

## One macOS installation

- The signed universal Plug.app is the supported macOS installation, delivered
  through the website/GitHub DMG or the `plug-app` Homebrew Cask.
- Opening the app once completes command-line setup, background-service
  registration, and any required macOS ServiceManagement and Keychain consent.
  A logged-in GUI session is required; headless macOS is not a supported
  installation path.
- The command-line symlink and MCP client configurations resolve to the
  executable inside the installed app. `plug link` and `plug repair` no longer
  create a second installed binary.
- Homebrew installs the app only. It does not install a separate Formula
  binary or write a competing command link.

## Safer migration and updates

- A single bounded reconciliation flow checks the app bundle, embedded binary,
  command link, client entries, launchd ownership, and daemon version before it
  reports a healthy installation.
- Recognized legacy Plug installations can be adopted: old command-line
  binaries, the old Homebrew Formula, legacy LaunchAgents, and client entries
  pointing at those paths are migrated only after Plug ownership is proven.
- Unknown files, launchd jobs, client entries, and credentials are preserved.
  Unrelated files are never overwritten or removed as part of migration.
- App-owned daemon replacement uses an exact-version IPC handshake and bounded
  process operations. Existing compatible adapters can reconnect and replay
  session state; incompatible versions receive an explicit compatibility
  result.
- Sparkle remains the macOS update path. After an app update, startup
  reconciliation repairs the canonical paths and brings the app-owned daemon
  to the new version through one workflow.

## Release and development changes

- macOS standalone CLI archives and the macOS shell installer path are retired.
  The installer refuses Darwin and points users to Plug.app. Linux retains its
  Homebrew Formula, shell installer, and standalone archives.
- The MCPB executable bundle is retired; normal client linking uses the same
  Plug runtime owned by Plug.app.
- Release metadata now comes from the Cargo workspace version. Release checks
  require the tag, app version, embedded `plug --version`, Sparkle appcast,
  Homebrew Cask, and monotonic app build number to agree.
- DMG, appcast, app-only Cask, Linux artifacts, and checksums are staged and
  published through one transaction so an incomplete publication cannot be
  promoted as the latest release.
- Fresh source development uses `./scripts/setup-codesigning.sh`,
  `./scripts/dev-reinstall.sh`, and `PLUG_DEV=1 plug-dev`. Development builds
  no longer replace the production `plug` command or Plug.app state.

## Upgrade notes

Open Plug.app after installing the DMG or Cask. On an existing macOS setup,
the app can reconcile recognized legacy Plug paths while leaving unrelated
files, jobs, client entries, configuration, OAuth state, and credentials
alone. Linux users keep their existing standalone installation choices.

These notes describe the integrated source and packaging contract. Installed
runtime migration, signed-artifact publication, and live routed-tool
certification remain separate release gates and are not claimed by this source
preparation.
