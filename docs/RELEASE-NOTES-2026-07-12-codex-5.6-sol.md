# Release notes, July 2026 reliability update - Codex 5.6 sol

This update is mostly about making Plug uneventful in the best way. Daemon
restarts, route refreshes, client disconnects, and slow upstreams now leave
far less state behind. The catalog path is faster, large results no longer
occupy an async worker while they are written, and OAuth state has tighter
on-disk protection.

No configuration migration is required. If you install a prebuilt binary,
the Rust version change does not affect you. Building from source now requires
Rust 1.88 or newer.

## Connections recover with their state intact

Daemon-backed clients now replay their capabilities, resource subscriptions,
and log level after reconnecting. A client that loses the daemon no longer
comes back as a half-initialized session. A 120-second read watchdog also
breaks a silent IPC stall and starts the normal reconnect flow instead of
leaving the session mutex stuck.

The SSE replay path keeps the unsent part of its queue if delivery fails. It
also re-checks the active sender before queueing, so a reconnect that lands at
the same time does not lose notifications.

## Resource subscriptions survive route refreshes correctly

Resource subscribe and unsubscribe calls now pass through one per-URI state
machine. That gives the upstream calls a defined order and closes several
races that could leave Plug believing a subscription was active when the
upstream had already removed it.

Plug records which upstream actually owns each subscription. When routing
changes, it drains the old owner and moves the subscription to the new one.
The refresh that introduced a route change also performs the reconciliation,
so recovery does not depend on another refresh happening later. Concurrent
rebind attempts share one result, and a failed move is returned to the client
instead of being reported as success.

## Tasks stop when their client leaves

Deleting an HTTP session, expiring an idle session, or disconnecting an IPC
client now stops its local task futures before forwarding cancellation to
task-capable upstream servers. Upstream cancellation runs concurrently and is
bounded by each server's call timeout, so one silent server cannot hold up all
other cleanup.

Task creation also checks the owner's lifecycle before publishing a task.
This prevents a slow create response from resurrecting a task after the
session has already gone away. Native task creation uses one deadline while
queueing the upstream request and waiting for its response. A saturated rmcp
request queue can no longer retain an owner guard forever.

## Reloads and reconnects agree on current configuration

Reconnect and restart commits now use the same reload lock as configuration
application. Plug connects outside the lock, then checks the current material
server configuration before installing the result. A reconnect started under
an older configuration is discarded instead of replacing the server selected
by a newer reload.

The config watcher now has real end-to-end coverage for direct writes, atomic
rename saves, invalid intermediate files, and unrelated files in the same
directory.

## OAuth storage is smaller and safer

Downstream OAuth now removes expired authorization codes and tokens during
normal mutations. Client-credentials requests reuse a live token for the same
scope set, including reordered or duplicated scopes, instead of minting a new
record every time.

Secret directories are created with mode `0700`. OAuth state writes remove a
stale temporary file before use, reject unsafe permission changes, and enforce
mode `0600` on the final file after rename. Persistence failures are logged
rather than disappearing silently.

The dependency graph also drops the duplicate default HTTP client pulled in
by `oauth2`, replaces the unmaintained `fs2` file lock with `fs4`, and pins
`rmcp` to compatible 1.7 releases.

## Faster catalog and artifact work

Catalog refresh now fetches resources, templates, and prompts at the same
time. Its hot loops reuse server lookups and skip filtered catalog builds when
filtering is disabled. Pagination reads from the existing catalog slice
instead of cloning the whole list first.

Payloads of at least 16 MiB are still written to the same artifact store, but
the file work now runs on Tokio's blocking pool. Large tool results therefore
do not occupy an async runtime worker during disk I/O.

## Internal cleanup that lowers future risk

The former 5,000-line daemon module is now a directory with separate modules
for wire framing, runtime paths, client registration, auth status, notification
delivery, and MCP dispatch. This was a move-only refactor. Callers and wire
behavior did not change.

Notification fan-out also shares one transport-independent classification
path across stdio, HTTP, and daemon IPC. This reduces the chance that a future
notification type behaves differently on one transport.

## Compatibility and verification

- No configuration migration is required.
- Prebuilt binaries have no new runtime dependency.
- Source builds require Rust 1.88 or newer.
- `rmcp` is constrained to the 1.7 release line.
- The merged workspace passes 857 tests: 620 library tests, 45 integration
  tests, and 192 binary tests.
- Clippy with warnings denied, formatting, Rust 1.88 compilation, RustSec
  advisories, and the todo-status guard all pass on `main`.

The technical execution record is in
[`plans/EXECUTION-REPORT-claude-fable.md`](../plans/EXECUTION-REPORT-claude-fable.md).
The last independent repair report is in
[`plans/FINAL-REPAIR-REPORT-codex-5.6-sol.md`](../plans/FINAL-REPAIR-REPORT-codex-5.6-sol.md).

## Known follow-ups

The shipped work closes the reviewed release blockers. A few narrower items
remain documented for later work: full live reconfiguration, an IPC `ping`
parity gap, JSON-RPC formatting for malformed HTTP request bodies, and a rare
cross-owner subscription supersede case. Resource subscription calls also use
the upstream connection's own completion behavior, so a permanently wedged
upstream can still block that URI's transition queue.
