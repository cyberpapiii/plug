# Plug for macOS — Product and Architecture Design

**Date:** 2026-08-25  
**Status:** Approved design  
**Target:** Plug 0.5.0  
**Minimum macOS:** 14.0

## Purpose

Plug needs two first-class interfaces over one runtime: a CLI shaped for agents and a native macOS app shaped for humans. Capability stays equal; idiom changes. The daemon remains the only runtime authority, owns every upstream connection, and persists every mutation. Neither interface maintains a competing model of configuration or health.

The product goal is calm operation. Healthy state should be understandable at a glance and visually quiet. Exceptions should explain themselves and offer one useful action. The app must not become another MCP client, intercept reverse requests, collect credentials, or duplicate Agent Admin.

## Product Surface

### Menu bar

One menu-bar glyph communicates aggregate state:

- healthy: visually recessive
- degraded: amber
- unavailable or incompatible: red

The menu shows server health, connected-client count, pending Plug-owned auth work, and four quick actions: open Plug, start or stop the daemon, restart after an update, and open logs. It never emits routine status notifications.

Notifications are coalesced by a stable event key so retries and flapping replace an existing notification instead of producing a stack. Four notification classes exist:

1. Upstream reauthentication required; opens the Auth view.
2. A downstream client requests a Plug OAuth grant; opens the existing browser/passkey consent flow. The notification is a doorbell, not an approval surface.
3. First-run daemon adoption is required; opens the migration dialog.
4. An installed app update needs a daemon restart; opens the one-action update state.

MCP elicitation and sampling remain pass-through reverse requests answered by the downstream client. Activity may record their routing metadata, but the app never renders an answer control.

### Main window

The window uses a compact `NavigationSplitView` with four primary destinations:

1. **Servers** is the default. Each row shows name, health, transport, tool count, auth state, and last error. Degraded rows sort first. Detail supports enable or disable, restart, edit, remove, and reauthentication. Adding a server accepts a command or URL, asks the daemon to validate it, previews the normalized result, then persists through a daemon verb.
2. **Clients** shows linked clients, live sessions, protocol era, and the effective tools each client can see. A searchable client-by-tool matrix answers visibility questions without a diagnostic command. Linking, unlinking, and repair live here.
3. **Activity** is an observation-only feed containing time, client, tool or method, upstream server, latency, and outcome. It never stores or displays parameters, results, prompt content, tokens, or secrets. Filters cover client, server, method, and failures.
4. **Auth** shows upstream OAuth state, expiry, and reauthentication actions; downstream registered clients and grants; owner-passkey enrollment state; and client revocation. Browser and passkey flows remain authoritative. The app opens them but never replaces them.

Secondary CLI capabilities remain available without crowding primary navigation:

- setup and import are part of onboarding and Add Server
- status and doctor appear as a concise diagnostics sheet
- tool inventory is reachable from Servers and Clients
- configuration validation, resolved configuration, raw-file reveal, logs, launch-at-login, and update settings live in Settings
- internal transport commands remain hidden

## Architecture

### Targets

The repository gains an Xcode project under `PlugApp/` with these targets:

- `PlugApp`: SwiftUI menu-bar app and main window
- `PlugIPC`: Swift package/library implementing typed daemon IPC
- `PlugAppTests`: model, formatting, compatibility, and action tests
- `PlugAppUITests`: small critical-flow suite

An Xcode project is preferred over a pure SwiftPM executable because the product needs a real app bundle, an embedded LaunchAgent, Sparkle, notifications, signing, hardened runtime, and notarized DMG packaging. Swift Package Manager remains the dependency mechanism, including Sparkle 2.

The app bundles the same release-built `plug` binary shipped by the CLI release. There is no separately implemented daemon or helper engine.

### Shared daemon API

Both CLI and GUI use typed, length-prefixed JSON over the existing owner-only Unix socket. New behavior is added as daemon IPC verbs rather than subprocess wrappers or direct file edits. Existing CLI command logic moves behind those verbs where needed, then both clients call the same operation.

IPC adds an explicit handshake containing:

- client kind and version
- supported IPC minimum and maximum
- daemon version and supported IPC range
- daemon ownership mode: app-managed or CLI-managed
- advertised operator capabilities
- actionable compatibility state

Compatibility uses a range, not exact version equality. An app below or above the supported range shows one upgrade action. A same-release app with an older bundled daemon offers a consented restart. A daemon older than the app's compatibility floor offers the matching CLI or app update path.

Operator mutations require the existing daemon auth token and socket ownership. Config writes happen inside the daemon using owner-only permissions, validation, and atomic persistence. The app never writes `config.toml`, token files, OAuth state, or runtime files directly.

The daemon exposes a bounded activity snapshot plus live event stream. It retains the newest 500 metadata-only events in memory and clears them on restart. No activity database, disk log index, analytics store, request body, or response body is added.

### Daemon ownership

Exactly one authority may start the daemon:

- when Plug.app is installed and its service registered, the embedded LaunchAgent owns the daemon
- in CLI-only installations, the CLI-managed LaunchAgent owns it

`SMAppService` registers the app's embedded `com.plug.daemon` LaunchAgent from `Contents/Library/LaunchAgents`. Its property list uses `BundleProgram` to reference the app-bundled `plug serve --daemon` executable by a bundle-relative path, preserving existing config, socket, token, OAuth, and log paths. `plug start` detects app ownership and asks launchd to kickstart the registered service; it never spawns a competitor. CLI-only `plug start` installs or repairs the CLI-owned service before kickstarting it.

The existing runtime file lock remains the final single-flight guard for all callers. Ownership selection prevents the race; the lock contains any remaining simultaneous start.

First launch audits current state: running process, socket, registered service, stale plist, binary path, and version. If migration is needed, one dialog explains that Plug.app will manage the daemon. Acceptance stops the old runtime gracefully, unregisters or supersedes the stale service, registers the embedded agent, starts it once, and verifies the socket and handshake. Failure stops after bounded retries and offers View Log; it never loops silently.

Monitoring uses bounded exponential backoff and a retry ceiling. Quitting Plug.app leaves the launchd-owned daemon running. Settings includes **Uninstall Plug…**, which calls `SMAppService.unregister()`, verifies that the daemon stopped, and then offers to move the app to Trash. Arbitrary Finder deletion cannot reliably run cleanup code, so Plug does not claim that drag-to-Trash alone unregisters the service; the registered service instead fails visibly and remains repairable by a reinstall or `plug doctor`.

Sparkle replacing the app does not forcibly restart a healthy daemon. After update, version skew produces one prompt: restart now to finish the update. Consent asks launchd to restart onto the new bundled binary and verifies the new handshake.

### App state model

One `@Observable` app model owns four explicit connection states:

- disconnected
- starting or adopting
- incompatible
- ready

Feature models consume immutable daemon snapshots and submit typed actions through `PlugIPC`. Views contain formatting and interaction only. AppKit bridges stay narrow: `NSWorkspace` for opening URLs, config, and logs; `UNUserNotificationCenter`; `SMAppService`; and Sparkle integration.

## Distribution and Updates

Plug.app supports macOS 14 and ships outside the Mac App Store. App Sandbox is intentionally disabled because the app supervises a user LaunchAgent, connects to the daemon Unix socket, opens operator-owned files, and bundles the daemon executable. Hardened runtime remains enabled.

The app is signed with the Developer ID Application identity, notarized, placed in a signed DMG, and both app and DMG receive stapled notarization tickets. The DMG is the recommended human installation path. CLI `.pkg`, Homebrew formula, archives, and shell installer remain available.

Sparkle 2 provides in-place updates. Its feed is the stable URL:

`https://github.com/cyberpapiii/plug/releases/latest/download/appcast.xml`

Every release attaches a newly generated full-history `appcast.xml`, including CLI-only releases, so the stable redirect never points to a release without a feed. CI generates the appcast and signs app updates with Sparkle EdDSA; it is never hand-edited. The EdDSA public key is embedded in the app. The private key is stored in Personal 1Password and a GitHub Actions secret, separate from the locally held Developer ID certificate.

The Homebrew tap adds a cask using the identical DMG and declares `auto_updates true`. No cask-specific build exists.

## Error Handling and Calm UX

- Healthy rows remain visually quiet; degraded and failed rows sort first.
- Each failure names the affected object, shortest useful cause, and one primary action.
- Start failures use bounded retries, then remain visible until the user retries.
- Version mismatch screens always include the correct update or restart action.
- Destructive actions use native confirmation with precise impact.
- No charts ship in 0.5.0. Current latency, last error, and recent activity answer the supported questions.
- No generic notification stream exists. Only the four Plug-owned classes notify.
- Empty states teach one action; they do not expose implementation terminology.

## Testing

Rust tests cover every new IPC request and response, authentication, atomic persistence, activity redaction and bounds, ownership arbitration, start-lock behavior, compatibility ranges, and old-client decoding.

Swift tests use a fake `PlugIPC` transport for all view models. They cover state reduction, sorting, filtering, coalescing identifiers, compatibility actions, failure backoff, and server/client/auth mutations. UI tests cover first launch, healthy overview, degraded server, client visibility, reauthentication routing, adoption, and update-restart prompts.

End-to-end tests run an isolated daemon with temporary config and socket paths. They prove CLI and GUI observe identical state and that mutations from either appear in the other. A macOS release gate installs the app in a clean test account, registers the LaunchAgent, adopts a stale CLI service fixture, restarts through launchd, and verifies Keychain-backed startup from a login session.

Release gates additionally verify:

- app and nested binary signatures
- hardened runtime and intended entitlements
- notarization acceptance and stapled tickets for app and DMG
- Sparkle EdDSA signature and full-history appcast
- DMG install and first launch
- Homebrew cask checksum and identical artifact digest
- CLI archive and package behavior remains unchanged

## Non-goals for 0.5.0

- answering MCP elicitation or sampling
- collecting upstream credentials or secrets in native fields
- replacing browser/passkey OAuth consent
- Agent Admin privilege approvals
- persistent activity history, analytics, charts, or cloud sync
- remote administration of another Mac
- Mac App Store distribution
- a second daemon implementation

## Delivery Sequence

1. Land daemon ownership, IPC handshake, activity stream, and shared operator verbs with CLI parity tests.
2. Build PlugIPC and Swift models against daemon fixtures.
3. Build menu bar, Servers, Clients, Activity, Auth, Settings, and first-run adoption.
4. Add notifications, Sparkle, DMG packaging, signing, notarization, and Homebrew cask.
5. Install on the owner Mac, migrate the stale service, verify real clients and upstreams, then release 0.5.0.

Modern MCP peer certification remains a separate release gate after the app work. Plug enables a modern direction only when an installed or reference peer passes the isolated matrix; otherwise the relevant gate stays off without affecting app release.
