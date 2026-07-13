# RMCP 2.2 upgrade - Codex 5.6 sol

Plug now uses RMCP 2.2.0, the latest stable release of the official Rust SDK
available on July 13, 2026. This is an SDK and compatibility upgrade, not a
change to the MCP version clients see: Plug still negotiates MCP `2025-11-25`.

No configuration migration is required. Existing stdio, Streamable HTTP,
daemon IPC, OAuth, Tasks, resources, prompts, completion, elicitation,
sampling, logging, and notification behavior remains available.

## Keychain prompt flood hotfix

This release also fixes the burst of macOS dialogs saying that Plug wants to
access the `plug` key in your login Keychain. The flood had three compounding
causes: OAuth tests were touching the real user credential stores, competing
daemon starts could initialize upstreams before discovering another daemon had
won, and routine credential loading checked Keychain before using the protected
on-disk token mirror.

- OAuth tests now use an isolated temporary token directory and in-memory
  credential store. Running the test suite cannot read or write your login
  Keychain or production token files.
- A daemon now claims the singleton lock before it starts any upstream server.
  Losing startup attempts wait for the winner instead of opening their own
  upstream connections or falling back to a duplicate standalone process.
- Normal OAuth startup uses the protected token mirror first. Keychain remains
  the recovery source when the mirror is missing and is still checked by
  explicit credential diagnostics.

No credentials or server configuration need to be recreated. The 868-test
keychain-hotfix suite was run while hashing the production token directory
before and after; its file count and content manifest remained unchanged. The
six dependency and runtime regressions bring the final release suite to 874 tests.

Local reinstalls are safer too. The installer now builds into a private staging
directory, signs the candidate with the stable `Plug Local Signing` identity,
strictly verifies that signature, smoke-tests the binary, and only then swaps it
into place atomically. A running client can therefore see either the previous
signed build or the new signed build, never an unsigned in-between build.

Read-only daemon auth status is now Keychain-safe as well. It reports from the
daemon's memory and protected token mirror instead of probing a mirrorless
Keychain entry. This prevents `plug auth status`, `plug status`, and unrelated
HTTP traffic from freezing behind a hidden macOS authorization dialog. Explicit
credential recovery and token-injection operations still retain the deliberate
Keychain fallback they need.

The release suite is also reliable on cold macOS runners again. Engine race
tests now launch the shared prebuilt MCP fixture directly instead of starting
several competing `cargo run` processes inside 30-second connection windows.
This removes build-lock timing from the concurrency assertions without relaxing
their timeouts or behavior checks.

## Dependency refresh

The rest of Plug's direct Rust dependencies were brought to their latest
compatible stable releases. The most important upgrades are Keyring 4.1.4,
Rand 0.10.2, TOML 1.1.2, and Tower HTTP 0.7.0; the refreshed lockfile updates
141 packages in total. No Plug configuration changes are required.

- macOS credentials retain the same login-Keychain service (`plug`) and account
  names, so the Keyring upgrade does not move or recreate saved OAuth tokens.
- Linux keeps the previous kernel-keyring naming scheme, so existing saved
  credentials remain discoverable there as well.
- Client discovery, config import, and `plug doctor` continue to parse complete
  TOML documents correctly under TOML 1.x.
- The HTTP request ceiling is now truly 4 MiB for both declared-length and
  streamed requests. Previously Axum's implicit 2 MiB limit could reject an
  otherwise valid request before Plug's configured limit was reached.
- The secure token generator and health-check jitter now use Rand 0.10's current
  APIs without weakening operating-system entropy or changing behavior.

All direct dependencies are current. Two older transitive crates remain because
their upstream dependents require those exact major versions; they are not
separately selectable by Plug and have no known advisory in the release gate.

## What improves

RMCP 2.2 includes the SDK's latest `2025-11-25` conformance fixes, stricter
S256 PKCE behavior, safer OAuth token and redirect handling, cancellation
corrections, and Streamable HTTP recovery fixes. Plug now builds directly on
the SDK's spec-aligned content, resource, prompt, task, and elicitation models
instead of RMCP 1.x compatibility types.

Cancellation is also safer at Plug's routing boundary. RMCP 2.2 correctly
models `notifications/cancelled` messages whose `requestId` is absent. Plug
accepts those messages but does not guess which request they refer to, so an
anonymous cancellation cannot stop an unrelated active call.

## What does not change

- The negotiated MCP revision remains `2025-11-25`.
- Client configuration and server definitions do not change.
- All three downstream paths remain supported: stdio, Streamable HTTP, and
  daemon IPC.
- Tasks keep their existing method names and wire shapes.
- Logging, Roots, and Sampling remain enabled for MCP `2025-11-25`, even
  though RMCP marks them deprecated in anticipation of future SEP-2577 work.
- The announced July 28 stateless MCP revision and its newer Tasks extension
  are not included in this upgrade. Downstream stdio and daemon-IPC clients
  requesting that revision are rejected instead of being told Plug supports it.

## Build compatibility

Source builds still require Rust 1.88 or newer. RMCP is pinned exactly to
`2.2.0`. `sse-stream` is pinned exactly to `0.2.4`, the release providing the
API RMCP 2.2 expects, so a fresh locked build cannot resolve the incompatible
older 0.2.2 API.

## Verification

The migration adds direct regression coverage for RMCP 2.2 resource-link JSON
and cancellation without a request id. Existing protocol-version, Tasks,
elicitation, sampling, and daemon IPC suites continue to exercise MCP
`2025-11-25` behavior. Additional regressions cover TOML 1.x document parsing,
Linux keyring compatibility, and the exact/streamed HTTP body limits. The
release gate covers all 874 workspace tests, Clippy with warnings denied,
formatting, Rust 1.88 compilation, RustSec advisories, dependency licensing and
sources, todo-status consistency, and clean diffs.
