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

## Fix Round: Shutdown and Socket Override Safety

- `FixtureConnectorReplay` now has explicit async teardown and bounded
  shutdown. It weakly captures reader/daemon closures, joins worker tasks,
  closes connections and pipes, terminates then kills the child if needed, and
  unlinks only its prefixed temporary socket.
- A repeated-run regression checks child reaping and socket removal. Three
  iterations of all four fixture tests passed with no test-owned child or
  socket left behind.
- `PLUG_SOCKET_PATH` is now accepted only when `PLUG_DEV=1`; production falls
  back to the normal runtime socket. Rust unit tests cover both branches.

## Fix Round Verification

- Full PlugApp: 74 passed.
- Repeated fixture: 12 passed (4 tests x 3 iterations).
- PlugIPC: 6 passed.
- `cargo test --workspace`: passed (864 `plug-core`, 247 `plug-mcp`).
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: still blocked by
  the two pre-existing findings named above.
