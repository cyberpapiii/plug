# Daemon-Owned HTTP Runtime Plan

**Goal:** Make the background `plug` service the primary runtime authority for both downstream
stdio proxy sessions and downstream HTTP sessions, while keeping standalone `plug serve` available
as an explicit fallback/debug mode.

## Why This Exists

The recent session-parity work made operator truth much better, but it still relied on an
aggregation seam between:

- daemon-owned IPC proxy state
- standalone `serve` HTTP session state

That is honest and safe, but still leaves two runtime truth sources. The next architecture step is
to make daemon mode own downstream HTTP directly so `plug start`, `plug connect`, `plug status`,
and related operator surfaces can query one shared runtime.

## Target Behavior

When the background service is running:

- the daemon owns the shared `Engine`
- the daemon owns the Unix socket IPC service
- the daemon owns the downstream HTTP/HTTPS server
- live session inventory comes from one process
- `ListLiveSessions` is transport-complete without runtime-side aggregation

Standalone `plug serve` remains available, but it is no longer the default runtime authority.

## Guardrails

- do not break existing standalone `plug serve` foreground behavior
- do not regress `plug connect` auto-start semantics
- do not make `plug status` or `plug doctor` lie during transition
- keep operator inventory/auth state explicit during mixed old/new daemon scenarios

## Execution Plan

### 1. Move daemon mode to build and own the HTTP runtime

Outcome:
- `cmd_daemon()` starts IPC + HTTP in one process

Acceptance:
- daemon startup builds the same HTTP runtime pieces used by standalone `serve`
- daemon-owned `ListLiveSessions` can include HTTP sessions directly

### 2. Make runtime inventory trust the daemon when it is authoritative

Outcome:
- operator surfaces stop double-counting or re-aggregating when daemon mode already has complete
  session truth

Acceptance:
- runtime fetch path returns daemon-provided `transport_complete` inventory directly
- standalone HTTP fallback is still used when daemon is unavailable or old

### 3. Clarify command semantics

Outcome:
- `plug start`, `plug serve`, `plug serve --daemon`, `plug connect`, and `plug status` describe the
  new ownership model clearly

Acceptance:
- help text and docs explain that background service now owns both IPC and HTTP
- standalone `serve` is explicitly described as foreground/fallback behavior

### 4. Expand verification

Outcome:
- one-runtime truth is pinned by tests and live CLI checks

Acceptance:
- tests cover daemon-owned `ListLiveSessions`
- tests cover command/runtime output assumptions that depend on transport-complete daemon truth
- full `cargo test -p plug -- --nocapture` passes

## Non-Goals

- removing standalone `plug serve`
- redesigning downstream OAuth semantics
- changing upstream server/auth behavior

## Success Criteria

- daemon mode owns both downstream transports
- normal operator surfaces can rely on daemon session truth directly
- standalone HTTP fallback remains available without becoming the product’s primary truth path

## Phase Status

- [x] daemon mode builds and owns the shared HTTP runtime
- [x] daemon `ListLiveSessions` can report transport-complete truth directly
- [x] runtime inventory trusts daemon-complete responses without double-counting
- [x] command/help/docs semantics distinguish shared background service from standalone foreground `serve`
- [x] focused and full `plug` test suite coverage passed for the new runtime model
