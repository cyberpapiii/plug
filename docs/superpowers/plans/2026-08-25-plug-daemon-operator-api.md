# Plug Daemon Operator API Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the CLI and Plug.app one authenticated, typed daemon API for state, mutations, activity, compatibility, and single-owner launchd lifecycle.

**Architecture:** Extend the existing length-prefixed JSON socket instead of adding HTTP or a second service. The daemon owns configuration persistence and operator state; the CLI becomes the first consumer of the new verbs. A small launchd ownership module chooses app-managed or CLI-managed startup before the existing runtime lock provides final single-flight protection.

**Tech Stack:** Rust, Tokio Unix sockets, Serde, launchctl, `SMAppService`-compatible LaunchAgent plists, existing Plug auth-token and atomic-config helpers.

**Spec:** `docs/superpowers/specs/2026-08-25-plug-macos-app-design.md`

## Global Constraints

- Preserve IPC decoding for protocol version 3 clients while adding an operator handshake version.
- Keep the daemon the only writer of Plug configuration and operator state.
- Never include request parameters, results, prompt text, OAuth tokens, or secrets in activity records.
- Retain at most 500 activity records in memory and nothing on disk.
- App-managed and CLI-managed launchd services must never compete.
- All start attempts end after bounded retries and expose a useful error.

---

### Task 1: Define the operator protocol and compatibility handshake

**Files:**
- Modify: `plug-core/src/ipc.rs`
- Modify: `plug/src/daemon/mod.rs`
- Test: `plug-core/src/ipc.rs`
- Test: `plug/src/daemon/mod.rs`

**Interfaces:**
- Produces: `OperatorHandshake`, `DaemonOwnershipMode`, `OperatorCapability`, `IpcRequest::OperatorHandshake`, and `IpcResponse::OperatorHandshake`.

- [ ] **Step 1: Write serialization and compatibility tests**

```rust
#[test]
fn operator_handshake_round_trips_with_compatibility_range() {
    let response = IpcResponse::OperatorHandshake {
        handshake: OperatorHandshake {
            daemon_version: "0.5.0".into(),
            ipc_min: 3,
            ipc_max: 4,
            ownership: DaemonOwnershipMode::AppManaged,
            capabilities: vec![OperatorCapability::ServerMutation],
        },
    };
    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["handshake"]["ipc_min"], 3);
    assert_eq!(serde_json::from_value::<IpcResponse>(json).unwrap(), response);
}
```

- [ ] **Step 2: Run the focused tests and verify the new types are missing**

Run: `cargo test -p plug-core ipc::tests::operator_handshake_round_trips_with_compatibility_range`

Expected: compile failure naming `OperatorHandshake`.

- [ ] **Step 3: Add the types and request/response variants**

```rust
pub const OPERATOR_IPC_MIN: u16 = 3;
pub const OPERATOR_IPC_MAX: u16 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonOwnershipMode { Unmanaged, CliManaged, AppManaged }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorCapability {
    ServerMutation, ClientMutation, AuthMutation, ConfigMutation, ActivityStream,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorHandshake {
    pub daemon_version: String,
    pub ipc_min: u16,
    pub ipc_max: u16,
    pub ownership: DaemonOwnershipMode,
    pub capabilities: Vec<OperatorCapability>,
}
```

Add `IpcRequest::OperatorHandshake { client_version: String, ipc_min: u16, ipc_max: u16 }` and `IpcResponse::OperatorHandshake { handshake: OperatorHandshake }`. It is a read-only owner-socket request and does not require the master token.

- [ ] **Step 4: Dispatch the handshake and test overlapping and incompatible ranges**

The daemon response uses `env!("CARGO_PKG_VERSION")`, the constants above, and the ownership detector introduced in Task 5. Until Task 5 lands, return `Unmanaged`.

Run: `cargo test -p plug-core ipc && cargo test -p plug-mcp daemon::tests::operator_handshake`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add plug-core/src/ipc.rs plug/src/daemon/mod.rs
git commit -m "feat: add operator IPC handshake"
```

### Task 2: Add one redacted activity ring

**Files:**
- Create: `plug-core/src/activity.rs`
- Modify: `plug-core/src/lib.rs`
- Modify: `plug-core/src/engine.rs`
- Modify: `plug-core/src/dispatch/mod.rs`
- Modify: `plug-core/src/ipc.rs`
- Modify: `plug/src/daemon/mod.rs`
- Test: `plug-core/src/activity.rs`

**Interfaces:**
- Produces: `ActivityStore::record(ActivityEvent)`, `ActivityStore::snapshot(ActivityFilter)`, `IpcRequest::ActivitySnapshot`, `IpcResponse::ActivitySnapshot`.

- [ ] **Step 1: Write bounds and redaction tests**

```rust
#[tokio::test]
async fn activity_store_keeps_only_newest_500_metadata_events() {
    let store = ActivityStore::default();
    for index in 0..501 {
        store.record(ActivityEvent::test(index)).await;
    }
    let events = store.snapshot(ActivityFilter::default()).await;
    assert_eq!(events.len(), 500);
    assert_eq!(events.first().unwrap().sequence, 2);
    assert!(!serde_json::to_string(&events).unwrap().contains("params"));
}
```

- [ ] **Step 2: Run the focused test and verify it fails**

Run: `cargo test -p plug-core activity::tests::activity_store_keeps_only_newest_500_metadata_events`

Expected: compile failure naming `ActivityStore`.

- [ ] **Step 3: Implement the bounded model**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEvent {
    pub sequence: u64,
    pub occurred_at_ms: u64,
    pub client: Option<String>,
    pub server: Option<String>,
    pub method: String,
    pub latency_ms: u64,
    pub outcome: ActivityOutcome,
}

#[derive(Default)]
pub struct ActivityStore {
    next_sequence: AtomicU64,
    events: RwLock<VecDeque<ActivityEvent>>,
    tx: broadcast::Sender<ActivityEvent>,
}
```

`record` assigns sequence/time internally and pops from the front while length exceeds 500. The API has no fields capable of storing payloads.

- [ ] **Step 4: Record one event at the shared dispatch boundary**

Wrap the dispatcher result timing once, derive only client identity, selected server, method, elapsed time, and success/error class, and record after completion. Do not add per-method logging.

- [ ] **Step 5: Expose a filtered snapshot over authenticated operator IPC**

Add `ActivitySnapshot { auth_token, after_sequence, limit, failures_only }`; clamp `limit` to 500 and require auth. Return `ActivitySnapshot { events }`.

Run: `cargo test -p plug-core activity dispatch && cargo test -p plug-mcp daemon::tests::activity_snapshot`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add plug-core/src/activity.rs plug-core/src/lib.rs plug-core/src/engine.rs plug-core/src/dispatch/mod.rs plug-core/src/ipc.rs plug/src/daemon/mod.rs
git commit -m "feat: add redacted daemon activity feed"
```

### Task 3: Move server and config mutations behind daemon verbs

**Files:**
- Create: `plug-core/src/operator.rs`
- Modify: `plug-core/src/config/mod.rs`
- Modify: `plug-core/src/ipc.rs`
- Modify: `plug/src/daemon/mod.rs`
- Modify: `plug/src/commands/servers.rs`
- Modify: `plug/src/commands/config.rs`
- Test: `plug-core/src/operator.rs`
- Test: `plug/src/commands/servers.rs`

**Interfaces:**
- Produces: `ServerDraft`, `ServerPatch`, `OperatorMutationResult`, `IpcRequest::{ValidateServer,AddServer,UpdateServer,RemoveServer,SetServerEnabled}`.

- [ ] **Step 1: Write tests for validation and atomic persistence**

```rust
#[test]
fn validate_server_does_not_write_config() {
    let fixture = OperatorFixture::new();
    let preview = fixture.validate(ServerDraft::http("search", "https://example.com/mcp"));
    assert_eq!(preview.normalized_name, "search");
    assert_eq!(fixture.config_bytes(), fixture.original_config_bytes());
}
```

- [ ] **Step 2: Run the test and verify the operator module is missing**

Run: `cargo test -p plug-core operator::tests::validate_server_does_not_write_config`

Expected: compile failure.

- [ ] **Step 3: Implement typed drafts and one atomic mutation function**

`apply_operator_mutation(config_path, mutation)` loads the current file, validates with existing config rules, writes through the existing owner-only atomic persistence helper, then returns the normalized object and whether reload is required. Do not build a second TOML editor.

- [ ] **Step 4: Add authenticated IPC variants and daemon dispatch**

All mutation variants include `auth_token`; update `requires_auth`, `extract_auth_token`, and redacted `Debug`. The daemon applies the mutation, calls `Engine::reload_config`, and returns `OperatorMutationResult`.

- [ ] **Step 5: Convert CLI server mutations to the daemon API**

Keep CLI parsing and human output. Replace direct persistence with the same IPC request the app will use. If the daemon is unavailable, start it through the canonical ownership path, then retry once.

Run: `cargo test -p plug-core operator && cargo test -p plug-mcp commands::servers`

Expected: PASS, including a parity test that CLI add is immediately visible in `Status`.

- [ ] **Step 6: Commit**

```bash
git add plug-core/src/operator.rs plug-core/src/config/mod.rs plug-core/src/ipc.rs plug/src/daemon/mod.rs plug/src/commands/servers.rs plug/src/commands/config.rs
git commit -m "feat: centralize operator mutations in daemon"
```

### Task 4: Add client and auth operator verbs

**Files:**
- Modify: `plug-core/src/ipc.rs`
- Modify: `plug-core/src/downstream_oauth/mod.rs`
- Modify: `plug/src/daemon/mod.rs`
- Modify: `plug/src/commands/clients.rs`
- Modify: `plug/src/commands/auth.rs`
- Test: `plug/src/daemon/mod.rs`

**Interfaces:**
- Produces: `IpcRequest::{OperatorSnapshot,RevokeClient,BeginUpstreamReauth,BeginDownstreamConsent}` and `IpcResponse::{OperatorSnapshot,OpenUrl}`.

- [ ] **Step 1: Write authorization and snapshot tests**

Test that every mutation rejects a missing or wrong master token, snapshot responses contain no bearer/refresh token fields, and a revocation is immediately visible in the next snapshot.

- [ ] **Step 2: Run the focused tests and verify variants are missing**

Run: `cargo test -p plug-mcp daemon::tests::operator_`

Expected: compile failure naming the new variants.

- [ ] **Step 3: Add a single dashboard snapshot**

The response contains server statuses, configured client links, live sessions, tool visibility summaries, upstream auth summaries, downstream grant summaries, owner-passkey enrollment, runtime version, and ownership mode. Reuse current status/auth/client inventory types; do not expose stored secrets.

- [ ] **Step 4: Add narrow mutation verbs**

`RevokeClient` calls the existing downstream OAuth revocation path. Reauth and consent verbs return a validated loopback or configured public URL for `NSWorkspace.open`; the app never receives a credential.

- [ ] **Step 5: Convert matching CLI paths and run parity tests**

Run: `cargo test -p plug-core --lib && cargo test -p plug-mcp --lib`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add plug-core/src/ipc.rs plug-core/src/downstream_oauth/mod.rs plug/src/daemon/mod.rs plug/src/commands/clients.rs plug/src/commands/auth.rs
git commit -m "feat: expose client and auth operator API"
```

### Task 5: Add deterministic daemon ownership and launchd control

**Files:**
- Create: `plug/src/service.rs`
- Modify: `plug/src/main.rs`
- Modify: `plug/src/runtime.rs`
- Modify: `plug/src/daemon/mod.rs`
- Test: `plug/src/service.rs`

**Interfaces:**
- Produces: `ServiceOwnership`, `ServiceState`, `service::inspect()`, `service::ensure_started()`, `service::stop()`, and `service::adopt_app_service()`.

- [ ] **Step 1: Write ownership precedence and command-plan tests**

```rust
#[test]
fn registered_app_service_wins_over_stale_cli_plist() {
    let state = ServiceFixtures::app_registered_with_stale_cli_plist();
    assert_eq!(select_ownership(&state), ServiceOwnership::AppManaged);
    assert_eq!(start_plan(&state), StartPlan::KickstartAppService);
}
```

- [ ] **Step 2: Run the focused test and verify the service module is missing**

Run: `cargo test -p plug-mcp service::tests::registered_app_service_wins_over_stale_cli_plist`

Expected: compile failure.

- [ ] **Step 3: Implement inspection and pure start planning**

Inspect socket reachability, runtime lock, PID/binary path, `launchctl print gui/$UID/com.plug.daemon`, app bundle presence, and CLI plist. Keep selection pure and command execution separate so tests need no live launchd changes.

- [ ] **Step 4: Implement bounded launchd actions**

Use `launchctl bootstrap`, `bootout`, and `kickstart -k` only after the pure plan selects them. Poll the socket at 100, 200, 400, 800, and 1,600 milliseconds, then return `ServiceStartError { log_path, last_state }`. Never spawn `plug serve` as a child.

- [ ] **Step 5: Route CLI start/stop and connect auto-start through the service module**

`plug start` prints `daemon managed by Plug.app` when appropriate. All paths retain the runtime file lock as the final daemon-side guard.

- [ ] **Step 6: Add an isolated fake-launchctl integration test**

Inject the command runner and filesystem roots. Prove three simultaneous starts yield one bootstrap/kickstart operation and all callers observe the same socket-ready result.

Run: `cargo test -p plug-mcp service runtime::tests::concurrent`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add plug/src/service.rs plug/src/main.rs plug/src/runtime.rs plug/src/daemon/mod.rs
git commit -m "feat: make launchd the daemon owner"
```

### Task 6: Complete the daemon API gate

**Files:**
- Modify: `docs/PROJECT-STATE-SNAPSHOT.md`
- Modify: `docs/PLAN.md`
- Create: `docs/solutions/architecture-patterns/daemon-operator-api-and-service-ownership.md`

**Interfaces:**
- Consumes: all prior tasks.
- Produces: verified daemon foundation for Plug.app.

- [ ] **Step 1: Run the complete repository gates**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo +1.88.0 check --workspace
cargo deny check advisories
scripts/check-todo-status.sh
```

Expected: every command exits 0.

- [ ] **Step 2: Run a CLI parity smoke test**

Start an isolated daemon with temporary config/runtime roots, call handshake, snapshot, server validation, add/remove, activity snapshot, and shutdown through IPC, and verify the CLI renders the same final state.

- [ ] **Step 3: Update current-truth documents and record the pattern**

Mark only verified code as done on main after merge. The solution note records why the daemon owns mutations and why launchd ownership selection precedes the runtime lock.

- [ ] **Step 4: Commit**

```bash
git add docs/PROJECT-STATE-SNAPSHOT.md docs/PLAN.md docs/solutions/architecture-patterns/daemon-operator-api-and-service-ownership.md
git commit -m "docs: record daemon operator foundation"
```
