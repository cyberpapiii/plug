# Task 7 Implementation Report

## Result

Task 7 now exercises signed-app reconciliation with an isolated, real stdio
adapter. The fixture launches the built embedded `plug` executable with
`connect`, frames daemon traffic over a temporary Unix socket, drops the daemon
connection, and proves the adapter reconnects and replays capabilities after
replacement.

## Behavior Proven

- Legacy adoption converges once; a second healthy retry preserves the
  installation snapshot, protected artifacts, mutating backend events, repair
  process calls, and connector replay/session evidence.
- Client repair writes and parses the exact temporary signed embedded
  executable path with `args == ["connect"]`; the old `CANONICAL` sentinel is
  rejected.
- The real adapter observes initial registration, daemon replacement, a new
  registration, and capability replay. The fixture injects only the Unix
  socket endpoint through `PLUG_SOCKET_PATH`; all daemon/config state remains
  temporary.
- Fresh installs repair the command without adoption, and interrupted Cargo
  cleanup remains retryable and idempotent.

## Verification

- Focused `UnifiedReconciliationFixtureTests`: passed.
- Repeated focused fixture (`-test-iterations 3 -run-tests-until-failure`):
  passed.
- Full PlugApp `xcodebuild test`: passed.
- `swift test --package-path PlugApp/PlugIPC`: 6 passed.
- `cargo test -p plug-mcp --quiet`: 245 passed.
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed.

## Concern

`cargo clippy -p plug-mcp --all-targets -- -D warnings` remains blocked by
pre-existing `repair_export_endpoint` dead-code and
`clippy::mut-range-bound` findings outside this Task 7 change.
