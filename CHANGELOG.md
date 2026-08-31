# Changelog

All notable changes to plug are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)
and the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Opening Plug on a healthy Mac no longer claims it is installing itself. Every
  launch begins by reading the installation, and that read-only pass was sharing
  its words with the phases that actually change it, so the menu bar greeted a
  working install with "Setting up… Finishing installation." It now says
  "Starting… Connecting to servers.", which is what is happening; only the
  phases that repair, replace, or clean up call themselves setup.
- A repair in progress describes itself instead of the situation it started
  from. The app kept a second copy of the installation state and refreshed it
  only when reconciliation finished, with a timer to reveal a generic notice
  while the copy was stale. Reading the coordinator's state directly retires
  both the copy and the timer.

## [0.8.8] - 2026-08-31

### Fixed

- Upgrading the daemon no longer strands a running `plug connect`. The IPC
  handshake refuses a mixed-version session, which is right, but a client that
  was already running when the daemon was replaced kept answering every call
  with `daemon reconnect failed: daemon/client version mismatch` forever, and no
  MCP host recovers from that on its own. A running process cannot re-exec
  itself into a newer binary, so the client now exits when it sees that
  mismatch on reconnect and lets the host spawn a fresh one from the installed
  binary. It exits only when the daemon is running the client's own executable
  file, which is exactly the case a respawn fixes; a client installed against a
  different copy of plug reports the mismatch instead, with a message saying a
  restart will not help, rather than exiting into a loop. A mismatch on the very
  first handshake still fails outright, since a process spawned moments ago is
  not the stale one.

## [0.8.7] - 2026-08-31

### Fixed

- The downstream OAuth issuer state lock now waits briefly before declaring a
  second writer. The lock is what keeps two processes from racing state
  publication, but a daemon restarted the moment its predecessor exits could
  find the lock still held by a descriptor the old process had not finished
  closing, and fail startup with an error that only a second restart cleared.
  Acquisition now retries for up to a quarter second; a genuinely live writer
  holds the lock far longer than that, so the guard keeps its meaning, and an
  I/O error still fails immediately rather than waiting out the retries. This
  also addresses a rare Linux CI failure in
  `revoke_sync_failure_degrades_lifecycle_and_restart_stays_revoked`, whose
  exact mechanism was never reproduced on macOS.

## [0.8.6] - 2026-08-31

### Fixed

- `plug doctor`'s `client_limits` check no longer guesses a tool count. It used
  to multiply the enabled-server count by an assumed ten tools each and warn
  whenever that product cleared a published client ceiling, which is wrong in
  both directions: thirteen small servers invented 130 tools and warned, while
  four large ones could hide a real 600 and pass. Doctor never starts a server,
  so it cannot know the total; what it can read from disk is which clients are
  pointed at plug. The check now reports a ceiling only for a client that is
  actually linked, and points at `plug status` for the real count. Windsurf (100)
  and VS Code Copilot (128) were both re-verified against their documentation on
  2026-08-30 and still stand.
- `plug doctor` names the 100-tool ceiling after the product that publishes it:
  Cognition acquired Windsurf, and the cap belongs to Cascade, now the legacy
  agent inside Devin Desktop. Devin Local, the agent that replaced it as the
  default, configures MCP through the Devin CLI and publishes no ceiling. The
  export target keeps the Windsurf name because the file it writes still does.
- The daemon no longer sends upstream credentials back over the IPC socket.
  `GetServerConfig` returned the stored `ServerConfig` whole, so every operator
  client read each upstream's bearer token and full environment in plaintext.
  Secrets are now replaced with a placeholder on the way out and restored from
  the stored config on the way back in, so editing a server in Plug.app keeps
  credentials it never received. `oauth_client_id` and `oauth_scopes` stay in
  the clear: a client ID is a public identifier and scopes are names, and the
  OAuth secret lives in the credential store, not in `ServerConfig`.

### Changed

- The leftover-path classify table is now pinned by
  `testdata/legacy_plug_programs.json` itself, not only its cases. Rust's
  `is_recognized_legacy_program` and Plug.app's `LegacyPlugProgram` each assert
  their table against the fixture, so a path shape added to one language without
  the other fails a test on both sides.
- `plug doctor` and uninstall cleanup no longer treat every launchd label ending
  in `.plug` as plug's own. `local.claude-rc.plug` runs `claude` with this repo
  as its working directory and was reported as an unknown plug job on every
  doctor run. A job qualifies now by sitting in the `com.plug.` label namespace
  or by running a binary named `plug`.

## [0.8.5] - 2026-08-30

### Fixed

- Each upstream's health task now starts as soon as that upstream's own start
  attempt settles, rather than after every upstream has settled. One slow server
  used to hold back every other server's first health tick: measured after a
  reboot, `imcp` took 55.7s to spawn, and two loopback upstreams that were ready
  and reachable 44 seconds earlier went unnoticed for that whole stretch.
  `ServerManager::start_all` reports each server as it resolves, and the bulk
  spawn that follows now only covers servers it never heard about.

## [0.8.4] - 2026-08-30

### Fixed

- An upstream that failed to start is now retried as soon as its health task
  begins instead of one full `health_check_interval_secs` later. The task
  consumed the interval's immediate first tick unconditionally, so a local
  upstream that was merely slow to bind its port after a reboot stayed down for
  the whole first interval even once it was ready: on a 60s interval this was
  measured at 122 seconds of avoidable downtime for two loopback servers at
  login. A server that started healthy still skips that first tick, since it
  was just contacted.
- `scripts/install-release.sh` now swaps the app bundle by rename instead of
  deleting it in place. Every MCP client's `plug connect` re-execs the bundle's
  binary, and macOS aborts a running process whose signed executable is
  unlinked underneath it, so installing 0.8.3 killed six live client processes.
  The superseded bundle waits outside `/Applications` until its last clients
  exit, and is swept on the next install.
- The release workflow moved to `actions/upload-artifact@v6` and
  `actions/download-artifact@v7`, the first majors that actually default to
  Node 24. The v5 majors advertised Node 24 support but still declared
  `runs.using: node20`, so the deprecation warning survived the first bump.

## [0.8.3] - 2026-08-29

### Fixed

- Plug.app no longer crashes while reconciling its install. ServiceManagement
  delivers the `SMAppService.unregister` reply on a background XPC queue, and
  the completion handler was written inside a `@MainActor` type, so the Swift 6
  toolchain the release runner uses treated it as main-actor isolated and
  trapped on an executor check the moment the reply arrived. The crash left the
  daemon unregistered and took every connected client down with it. The handler
  is now explicitly `@Sendable`, which is what the callback contract has always
  been.

### Changed

- `scripts/release.sh` takes a finished change all the way to a release
  installed on this Mac: version bump, changelog heading, pull request,
  auto-merge, tag, release build, signed install, and the build-cache sweep.
  `scripts/install-release.sh` does the install half on its own, verifying the
  DMG against the release checksums and refusing anything Gatekeeper rejects.
- CI and release workflows moved to the artifact, cache, and Node actions that
  run on Node 24. The Node 20 versions are deprecated and were already being
  forced onto a newer runtime.

## [0.8.2] - 2026-08-29

### Fixed

- The CLI now allows a cold daemon start the same 90 seconds Plug.app allows,
  and waits instead of force-restarting when another process already holds the
  daemon's runtime lock. Both halves drive one daemon; the CLI's previous
  8-second budget meant a `plug connect` could kill the cold start Plug.app was
  waiting on, and a second client would kill the next one.
- Operator requests and `plug connect` session setup now time out instead of
  waiting forever. A daemon whose engine is stuck still accepts connections, so
  every status command and every connecting client used to hang with nothing
  reported.
- A crashed daemon is restarted by launchd. Both plists now carry
  `KeepAlive`/`SuccessfulExit=false`, so a panic or a kill self-heals while idle
  grace-period shutdown and `plug stop` still stay stopped.
- A fatal daemon startup error is written to the daemon log. It previously
  reached only stderr, which the app-owned LaunchAgent redirects nowhere.
- Sign-in and sign-out in Plug.app no longer deadlock on a talkative CLI. They
  ran `plug auth` through their own copy of the process-running code, which
  waited for the child to exit before reading its output; a child that filled
  the 64 KB pipe buffer blocked on the write while the app blocked on the wait.
  Both now go through `ProcessRunner`, the app's one process implementation,
  which drains both pipes concurrently and tears down the whole process group
  when a command runs long.
- A daemon that cannot read its own launchd registration now blocks instead of
  reporting repairable drift. `unknown` ownership is an absence of evidence, and
  drift is retried on every trigger, so the app kept walking the adoption path
  against a daemon nobody had proved was its own.
- Booting a legacy launchd job out now re-reads the job's program path first.
  `launchctl bootout` addresses a job by label, and a label reused by a
  different program between inspection and teardown would have been booted out
  on the strength of the earlier job's evidence.
- Pausing downstream connectors no longer blocks the main thread. It shelled out
  to `ps` synchronously with no timeout, so a wedged `ps` froze the whole app;
  it now goes through `ProcessRunner` with the rest.
- A status command can no longer defeat a daemon start. Reading the runtime lock
  takes it and lets it go again, and a start that collided with that momentary
  hold failed outright with "another plug daemon is already running". The start
  now outwaits a probe-length hold before drawing that conclusion.
- `plug`, `plug client list`, and `plug servers` now report the daemon as
  starting rather than stopped while a cold start is in flight. The socket is
  bound only once every upstream is up, so for tens of seconds a healthy daemon
  looked absent, which invited a repair that fought the start.
- One unreachable server can no longer hold up every other server's startup.
  The HTTP and legacy SSE upstream clients were built without a connect
  timeout, so a host that never answers ran until the per-server start timeout
  expired; both now use a ten-second connect bound. OAuth metadata discovery on
  the start path is bounded too, at a sixth of that server's own start budget,
  because the client rmcp builds for it carries a thirty-second timeout and no
  connect bound — exactly the default start timeout, so one unreachable OAuth
  host could consume a server's entire budget. A recorded cold start took
  32.65 s across thirteen servers, of which one server spent 30.18 s inside
  discovery.

### Changed

- Debug and test builds now emit line tables instead of full DWARF, and
  dependencies emit no debug info at all. Panic backtraces still resolve to
  file and line. A cold `cargo build --workspace --all-targets` drops from 40
  to 31 seconds and from 5.05 GB to 3.34 GB.
- Plug.app asks the daemon for the tool list only when it would answer
  differently. The daemon now reports a tool catalog revision on the cheap
  status snapshot; the app used to refetch the whole catalog on a fifteen-second
  timer because it assembled its own fingerprint from server fields and so could
  not see a tool disabled from the CLI. The app also reuses the handshake it
  already negotiated on an open connection instead of renegotiating on every
  poll.
- The operator status snapshot no longer carries upstream branding icons. An
  icon is a base64 data URI, and two servers advertising large ones were about
  half of a snapshot that is polled every couple of seconds. Tool listings still
  carry icons, which is where a client renders them, and `plug servers --output
  json` is unchanged. Together with the catalog change, thirty seconds of app
  polling drops from roughly 1918 KiB across 45 round trips to roughly 262 KiB
  across 30.

## [0.8.1] - 2026-08-26

### Fixed

- Let an app-managed daemon finish one bounded cold start instead of force-
  restarting it every 250 milliseconds while its upstream servers initialize.

## [0.8.0] - 2026-08-26

### Added

- Rebuilt Plug.app around a useful menu-bar popover, with one plain-language
  status, direct repair actions, live server state, connected-app visibility,
  Settings, and Quit in immediate reach.
- Added full Servers, Tools, Connections, and Activity workspaces, including
  searchable tool names, per-tool switches, server editing, app linking,
  server import, remote-client revocation, sign-in and sign-out, and readable
  call attribution.
- Added native macOS 26 Liquid Glass for compact controls and transient
  surfaces, with an accessible material fallback on macOS 14 and 15.

### Changed

- Replaced protocol and service jargon with calm, human language and made
  state readable by symbol and text rather than color alone.
- Refreshes stay responsive while the app is visible, slow down in the
  background, fetch activity incrementally, and avoid reloading the complete
  tool catalog every two seconds.
- Operator IPC v6 lets the signed app load a complete server definition before
  editing, so compact edits preserve advanced settings and credentials.

### Fixed

- Editing a server no longer replaces fields the form did not display with
  empty defaults.
- Activity history now returns the newest bounded calls instead of the oldest
  calls in the retained ring.
- PlugApp's architecture check now runs as a shell CI gate instead of reading
  source files from the signed XCTest host, removing a macOS privacy hang from
  the local test loop.
- Recognize the exact bundle-relative ServiceManagement daemon as app-owned
  when its recorded build is older, then unregister the old app service before
  registering the replacement. Updates no longer strand a valid installation
  behind stale background-service evidence.

## [0.7.5] - 2026-08-25

### Fixed

- Make Plug.app the sole macOS daemon starter whenever a verified app is
  installed. `plug connect` now opens Plug.app for recovery instead of
  recreating the legacy command-line LaunchAgent.
- Automatically reclaim missing or legacy daemon ownership after the user has
  already enabled Plug.app's ServiceManagement agent. First-run adoption still
  requires the original explicit consent.
- Reject production `plug serve --daemon` processes launched outside the
  app-owned launchd job, preventing external supervisors from becoming a
  competing runtime owner. `PLUG_DEV=1` and app-free installs retain their
  development and Linux behavior.

## [0.7.4] - 2026-08-25

### Fixed

- Ignore unrelated launchd jobs that disappear between broad discovery and
  inspection, while preserving fail-closed inspection for Plug's exact daemon
  label.
- Exclude Plug.app's own RunningBoard application job from daemon ownership
  evidence, so the open app cannot block adoption of its background service.
- Preserve the verified `~/.local/bin/plug` shell-link location as legacy
  launchd evidence, allowing the app to adopt the old CLI-managed daemon.
- Stop counting the canonical `~/.local/bin/plug` link as a competing
  install once it points at the bundled executable, so a fully repaired
  installation no longer shows a false "did not converge" warning.
- Name the exact final check that disagreed in the installation drift banner
  instead of listing every possible cause.

## [0.7.3] - 2026-08-25

### Fixed

- Ignore unrelated launchd jobs whose labels merely contain `plug`; only proven
  Plug ownership participates in daemon adoption. The exact
  `com.plug.daemon` label and jobs with Plug executable or ServiceManagement
  evidence remain fail-closed.

## [0.7.2] - 2026-08-25

### Fixed

- Fixed first-run reconciliation when the daemon is stopped. The app now
  accepts Doctor's valid machine-readable failure report and continues into
  daemon adoption instead of stopping before startup.

## [0.7.1] - 2026-08-25

### Fixed

- Fixed an installed-app delegation loop that could make the embedded CLI time
  out while verifying its own version. Plug.app now performs that internal
  version probe without re-entering app discovery.

## [0.7.0] - 2026-08-25

Detailed notes: [Plug 0.7.0](docs/RELEASE-NOTES-0.7.0.md).

### Added

- A bounded macOS installation coordinator that reconciles the signed app,
  embedded daemon, command link, MCP client entries, launchd ownership, and
  runtime version as one installation.
- Recognition and conservative migration of supported legacy Plug binaries,
  Homebrew Formula installs, LaunchAgents, and client paths.

### Changed

- Plug.app is now the sole supported public macOS owner of the GUI, `plug`
  command, background daemon, client links, and Sparkle updates. The Homebrew
  Cask installs the app without a competing command binary.
- macOS client linking, command delegation, and repairs resolve the verified
  app executable. Source development is isolated to `plug-dev` with
  `PLUG_DEV=1`; Linux keeps standalone Formula, shell-installer, and archive
  paths.
- Release packaging stages the DMG, signed appcast, app-only Cask, Linux
  artifacts, and checksums through one publication transaction governed by a
  single workspace version.

### Fixed

- App-owned daemon updates now use bounded ownership checks, exact-version IPC
  handshakes, and safe replacement/reconnect behavior, including session replay
  for compatible adapters.
- Recognized Plug state can be repaired without overwriting unrelated files,
  launchd jobs, client entries, configuration, or credentials.

### Removed

- macOS standalone CLI release artifacts and the executable MCPB bundle, which
  could create a second runtime owner.

### Documentation

- Clarified supported installation paths: one Plug.app on macOS from the
  website/GitHub DMG or Homebrew Cask (open it once), Linux Formula/shell/archive
  installs, and isolated source development through `PLUG_DEV=1 plug-dev`. Fresh source
  setup now runs `./scripts/setup-codesigning.sh` before
  `./scripts/dev-reinstall.sh`; development invocations use
  `PLUG_DEV=1 plug-dev`. Plug.app owns the macOS GUI, command line, daemon,
  client links, and updates; headless macOS is unsupported.

## [0.5.2] - 2026-08-25

Detailed notes: [Plug 0.5.2](docs/RELEASE-NOTES-0.5.2.md).

### Fixed

- Corrected the embedded LaunchAgent's executable argument so macOS can start the app-owned daemon.
- Made daemon adoption pause legacy connectors during the one-time handoff, wait for the previous process to exit, and require a real IPC-ready daemon before reporting success.
- Made both the app and CLI recognize the real `SMAppService` launchd record, preventing the CLI from replacing app ownership when the daemon is temporarily unavailable.

## [0.5.1] - 2026-08-25

Detailed notes: [Plug 0.5.1](docs/RELEASE-NOTES-0.5.1.md).

### Fixed

- Prevented the native app from crashing when macOS completes notification authorization on a background queue.
- Made first-run daemon adoption recognize and replace stale or legacy LaunchAgents, stop an unmanaged older daemon gracefully, and restart the app-owned daemon deterministically.
- Prevented test runtimes from ever registering their temporary binaries as the real macOS background service.

## [0.5.0] - 2026-08-25

Detailed notes: [Plug 0.5.0](docs/RELEASE-NOTES-0.5.0.md).

### Added

- A native macOS 14+ menu-bar app with calm health status, server controls,
  connected-client visibility, a redacted activity feed, upstream OAuth repair,
  and settings. The app is a full client of the same daemon used by the CLI.
- A versioned, redacted operator IPC surface for the app: compatibility
  handshake, server/client snapshots, bounded activity history, server
  mutations, and downstream-client revocation.
- LaunchAgent ownership through `SMAppService`, including first-run adoption of
  older CLI-managed installations and a clear restart action after app updates.
- Signed, notarized, and stapled universal `.dmg` distribution, Sparkle 2
  updates with a signed stable appcast, and a Homebrew cask using the identical
  disk image.
- Native notifications for upstream reauthorization and newly authorized
  downstream clients, coalesced so retries and flapping servers cannot spam the
  user.
- A complete official prerelease MCP `2026-07-28` server conformance fixture and
  durable evidence for 22 passing checks with zero failures.

### Changed

- Daemon startup is now single-owner and launchd-managed. The CLI, app, and
  reconnecting clients share one arbitration path instead of competing to spawn
  child daemons.
- Live server edits now flow through daemon verbs and atomic config persistence,
  so the app and CLI cannot create two sources of truth.
- `enable_prefix = false` now does what the configuration promises. Unique tools,
  resources, templates, and prompts pass through unchanged; collisions alone
  fall back to server-qualified names.

### Fixed

- Modern request-scoped progress survives metadata translation, maps RMCP's
  upstream token back to the client's token, and streams on the finite HTTP POST
  response before the final result.
- Concrete URIs expanded from advertised resource templates now route to the
  correct upstream server, with ambiguous cross-server matches rejected.
- The installed app and daemon negotiate an IPC compatibility range and offer a
  useful update/restart action instead of failing opaquely on version skew.

### Security

- The operator activity feed is bounded and redacted at capture time; tool
  arguments, results, credentials, and raw prompts never enter app telemetry.
- Sparkle's EdDSA update signature and Apple's Developer ID signature provide
  independent verification of app updates. Missing signing material fails the
  release before any unsigned artifact can be published.

## [0.4.0] - 2026-08-24

Detailed notes: [MCP 2026 dual-era modernization](docs/RELEASE-NOTES-2026-08-04-MCP-2026-DUAL-ERA-MODERNIZATION-codex-5.6-sol.md), [multi-client OAuth](docs/RELEASE-NOTES-2026-07-17-MULTI-CLIENT-OAUTH-codex-5.6-sol.md), [RMCP 2.2 upgrade](docs/RELEASE-NOTES-2026-07-13-RMCP-2.2-codex-5.6-sol.md), and [July 2026 reliability update](docs/RELEASE-NOTES-2026-07-12-codex-5.6-sol.md).

### Added

- Standards-based downstream OAuth for multiple public MCP clients, including RFC 7591 Dynamic Client Registration, OAuth Client ID Metadata Documents, explicit consent, PKCE S256, resource-bound tokens, and client list/revoke commands.
- End-to-end config watcher coverage for normal saves, atomic-renames, parse failures, and unrelated file changes.
- IPC proxy characterization coverage for reconnects, retries, malformed frames, notification ordering, and replayed session state.
- CI checks for the declared Rust 1.88 minimum version, RustSec advisories, and todo-file status consistency.
- Opt-in MCP `2026-07-28` downstream and upstream protocol adapters, with independent global gates and a per-server `legacy`, `auto`, or `modern` negotiation policy.
- Modern task lifecycle support with principal-scoped ownership, retrieval, cancellation, expiry, and disconnect-safe execution.
- Secure native modern-to-modern multi-round tool continuations using integrity-protected, principal-bound, expiring, single-use request state.
- A bounded extension envelope that preserves admitted protocol metadata and W3C trace context without allowing unknown fields to influence authorization, identity, routing, credentials, or continuation state.

### Changed

- Remote MCP clients now connect with only Plug's `/mcp` URL and receive isolated registrations and grants. The old singular client ID, shared secret, and redirect allowlist configuration are intentionally removed; existing remote clients authorize once after upgrading.
- Reconnecting daemon clients now restore capabilities, resource subscriptions, client log level, and other session state before resuming work.
- Catalog refresh fetches resources, templates, and prompts concurrently and avoids repeated server lookups and unnecessary filtered views.
- Oversized artifact writes run on the blocking pool instead of occupying an async runtime worker.
- Native task creation and task teardown now use bounded waits derived from each upstream server's call timeout.
- Split the daemon implementation into focused framing, path, registry, auth, notification, and MCP dispatch modules without changing its public behavior.
- Source builds now require Rust 1.88.
- Upgraded the Rust MCP SDK from RMCP 1.7.0 through 2.2.0 to exactly RMCP 3.1.0. The new protocol path remains default-off while legacy behavior stays available for current clients and servers.
- Migrated to RMCP's spec-aligned content, resource, prompt, task, elicitation, and cancellation APIs.
- Refreshed every direct Rust dependency to its latest compatible stable release, including Keyring 4.1.4, Rand 0.10.2, TOML 1.1.2, and Tower HTTP 0.7.0.

### Fixed

- Full default OAuth grants now admit ordinary tool calls to modern upstreams by including the continuation permission Plug must reserve before the first round can cause side effects.
- Resource subscriptions now serialize upstream transitions per URI, preserve the correct recorded owner, and heal route changes without false success or zombie registry entries.
- HTTP and IPC session teardown now aborts local task execution and forwards bounded cancellation to task-capable upstreams.
- Task creation can no longer recreate records after the owning session has been removed, leak owner guards behind a full request queue, or lose cancellation in the send-to-record window.
- Reloads and reconnects now commit through the same coordination lock, so stale reconnect attempts cannot overwrite newer configuration.
- SSE replay preserves the unsent tail after a delivery failure and no longer clears a sender installed by a racing reconnect.
- Daemon IPC read silence now forces a reconnect instead of holding the session mutex indefinitely.
- Replacement grace tasks now participate in shutdown and a shutdown signal remains latched even when no receiver is present.
- Fixed expired-session counter underflow, pending cancellation replay, a daemon reverse-request busy loop, and closed-channel restoration after deregistration.
- Cancellation notifications without `requestId` are accepted and ignored safely instead of being mapped onto an unrelated active call.
- Downstream stdio and daemon-IPC initialization reject RMCP's announced-but-unimplemented MCP `2026-07-28` revision instead of accidentally negotiating it.
- Pinned `sse-stream` 0.2.4 to match the API required by RMCP 2.2.0 and keep fresh locked builds reproducible.
- Preserved complete TOML document parsing after the TOML 1.x upgrade for client discovery, imports, and doctor checks.
- Made Plug's documented 4 MiB HTTP request limit authoritative instead of Axum's hidden 2 MiB default.
- Local macOS reinstalls now sign and verify a staged binary before atomically replacing the live executable, eliminating the unsigned execution window that could retrigger Keychain prompts.
- Daemon auth-status queries no longer fall back to a missing token mirror's Keychain entry, preventing a read-only diagnostic from freezing IPC and HTTP behind a macOS authorization dialog.
- Engine concurrency tests now launch the prebuilt mock server directly, avoiding parallel `cargo run` lock contention that could exhaust their startup timeout on macOS CI.
- Unbound legacy OAuth credentials now require explicit reauthorization instead of being silently rebound to a newly discovered issuer.
- Startup rejects an unbound legacy OAuth file before probing Keychain, avoiding authorization prompts for credentials that cannot be admitted.
- Modern duplicate in-flight JSON-RPC IDs are rejected atomically, with cancellation and cleanup tied to the exact admitted call.
- Expired durable tasks abort local work and forward bounded upstream cancellation before releasing their quota.
- Authorization-required upstreams now produce a distinct machine-readable protocol outcome rather than a generic unavailable-server error.
- Modern Host validation accepts the configured public URL without weakening unrelated-origin checks, and protocol mismatch responses consistently identify the selected MCP revision.
- Failed stdio discovery probes no longer latch the modern era before a successful discovery response.
- MCP conformance selectors now fail if they match zero tests.

### Security

- Downstream authorization codes, access tokens, refresh tokens, redirects, revocation, quotas, and expiry are isolated by client; registration never grants tool access, and all durable OAuth state is written atomically with owner-only permissions.
- OAuth secret directories are created with owner-only permissions.
- Downstream OAuth state persistence fails closed on unsafe temporary-file permissions and enforces owner-only permissions after rename.
- Expired OAuth records are swept, equivalent scope sets reuse tokens, and client-credentials requests reuse live tokens instead of growing the store on every call.
- Replaced the unmaintained `fs2` lock dependency with `fs4` and removed the duplicate default HTTP stack from `oauth2`.
- Modern continuation state is authenticated and bound to the initiating principal, request, and route, with expiration, replay prevention, bounded storage, and revocation cleanup.

### Known limitations

- `modern_upstream_enabled` and `http.modern_downstream_enabled` default to `false` until real-peer conformance evidence supports changing the defaults.
- Modern downstream negotiation is gated independently across HTTP, stdio, and daemon IPC; existing clients still negotiate the legacy lifecycle by default.
- `subscriptions/listen` is not advertised yet, even though ownership and quota foundations exist.
- Legacy-downstream calls into modern upstream tools, modern-downstream calls into legacy upstream multi-round tools, and task-plus-modern-upstream calls are suppressed rather than risking a stranded request.
- MCP Apps/UI capabilities and synthesized list-result cache directives are not advertised. Admitted metadata can still travel as opaque, policy-limited data.

## [0.3.0] - 2026-05-17

### Added

- SSE reconnect replay for downstream Streamable HTTP sessions.
- Daemon IPC resource subscribe/unsubscribe and targeted resource update delivery.
- Operator source/trust metadata and clearer upstream-vs-inferred tool risk annotations.
- Trace correlation across downstream requests, router calls, retries, reconnects, and upstream HTTP proxying.
- SEP-2243 `Mcp-Method` / `Mcp-Name` validation and upstream header emission.
- Current server-card discovery at `/.well-known/mcp-server-card` with the legacy `/.well-known/mcp.json` alias preserved.
- RFC 9728 protected-resource metadata and client-credentials downstream OAuth support.
- Optional macOS stdio upstream sandboxing.
- Public crates.io packages under `plug-core` and `plug-mcp`.
- Build artifact cleanup helpers for local release and reinstall workflows.

### Changed

- Upgraded `rmcp` to `1.7.0`.
- Replaced the deprecated `serde_yml` parser with `serde_norway`.
- Updated public distribution metadata to the `cyberpapiii/plug` repository and `cyberpapiii/homebrew-tap`.
- Made `cargo install plug-mcp --locked` the primary public Cargo install path.

### Fixed

- Removed obsolete protocol-version response rewrite internals while preserving remote-client compatibility.
- Hardened OAuth discovery/challenge behavior and refresh-token handling.
- Kept daemon, HTTP, and stdio capability surfaces aligned after the hardening pass.

## [0.1.0] - 2026-03-04

### Features

- **core**: MCP multiplexer — shared upstream sessions, 4-tier tool routing
- **transport**: stdio transport for Claude Code, Cursor, Codex, Gemini CLI, and all MCP clients
- **transport**: streamable-HTTP + SSE transport with session management
- **transport**: DNS-rebinding prevention via Origin header validation
- **routing**: prefix-based tool routing (`servername__toolname` convention)
- **routing**: client-aware tool filtering (Cursor ≤40, Windsurf ≤100, VS Code ≤128)
- **routing**: fan-out tool calls with merge and conflict resolution
- **resilience**: circuit breaker per upstream server with half-open recovery
- **resilience**: exponential backoff with jitter on transient failures
- **resilience**: health checks with configurable intervals
- **config**: TOML configuration with layered overrides (file → env → CLI)
- **daemon**: headless daemon mode with PID file and lock management
- **http**: `GET /.well-known/mcp.json` server discovery card endpoint
- **cli**: `plug connect`, `plug status` commands (TUI surface later removed; CLI-first)
- **dist**: single binary, zero runtime dependencies

[Unreleased]: https://github.com/cyberpapiii/plug/compare/v0.7.2...HEAD
[0.7.2]: https://github.com/cyberpapiii/plug/compare/v0.7.1...v0.7.2
[0.7.1]: https://github.com/cyberpapiii/plug/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/cyberpapiii/plug/compare/v0.6.4...v0.7.0
[0.5.2]: https://github.com/cyberpapiii/plug/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/cyberpapiii/plug/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/cyberpapiii/plug/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/cyberpapiii/plug/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/cyberpapiii/plug/releases/tag/v0.3.0
[0.1.0]: https://github.com/cyberpapiii/plug/releases/tag/v0.1.0
