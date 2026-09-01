---
title: "feat: Phase 3 Resilience and Token Efficiency"
type: feat
status: active
date: 2026-03-03
---

# feat: Phase 3 Resilience and Token Efficiency

> Historical planning note: This file is implementation history from 2026-03-03. It is not a
> current-state source of truth. Use `docs/PROJECT-STATE-SNAPSHOT.md` and `docs/PLAN.md` for the
> current project state.

## Enhancement Summary

**Deepened on:** 2026-03-03
**Sections enhanced:** Architecture, Circuit Breaker, Health Checks, Concurrency, Tool Filtering, Reconnection
**Research agents used:** architecture-review, performance-oracle, rmcp-tool-research, circuit-breaker-patterns, backon-api-research

### Key Architectural Corrections From Research

1. **Split state storage**: Health/circuit state in `DashMap` on `ServerManager`, NOT grouped with tool cache in ArcSwap (different update frequencies; health updates every 60s would clone entire tool list)
2. **Pre-cache filtered tool lists** per client type in `RouterSnapshot` — `list_tools_for_client()` becomes O(1) at request time instead of O(n log n)
3. **Circuit breaker + Semaphore on ServerManager** as `DashMap<String, Arc<T>>` — survives reconnects and ArcSwap swaps
4. **Lock-free circuit breaker**: `AtomicU64` for timestamps instead of `Mutex<Option<Instant>>`, `Semaphore` for half-open probes
5. **RwLock not OnceLock** for `ProxyHandler::client_type` — OnceLock silently discards second `set()`, wrong for daemon mode
6. **backon v1.6 API** requires explicit `.sleep(tokio::time::sleep)` — not auto-detected
7. **Trigger `refresh_tools()` after reconnect** — new server may have different tools

## Overview

Phase 3 adds resilience (circuit breakers, health checks, concurrency limiting) and token efficiency (client-aware tool filtering, description truncation, tool search) to the plug MCP multiplexer. This builds on Phase 1 (stdio proxy, PR #1) and Phase 2 (HTTP transport, PR #2). The goal: plug handles upstream failures gracefully, respects per-client tool limits, and minimizes token overhead.

## Problem Statement

Currently plug has no resilience layer:
- If an upstream server hangs, `call_tool` blocks indefinitely (no timeout)
- If an upstream crashes, its tools remain in the cache until restart
- `ServerHealth::Degraded` and `ServerHealth::Failed` are defined but never assigned
- `ProtocolError::Timeout` exists but is never constructed
- All clients get all tools regardless of their limits (Windsurf 100, VS Code Copilot 128)
- No tool description truncation or optional field stripping for token savings
- `client_type` is detected in `initialize()` but immediately discarded

## Proposed Solution

Seven subsystems organized into three implementation sub-phases:

**Sub-phase A (Foundation)**: Health checks + concurrency limiting + config
**Sub-phase B (Resilience)**: Circuit breakers + tool call timeout + reconnection
**Sub-phase C (Efficiency)**: Client-aware filtering + token efficiency + tool search

## Technical Approach

### Architecture

**CORRECTED** (per architecture + performance review): Health/circuit/semaphore state lives on `ServerManager` in `DashMap` containers, NOT in ArcSwap with tool cache. Rationale: health updates every 60s per server would clone the entire tool list if grouped; DashMap gives fine-grained lock-free updates (same pattern as `SessionManager`).

Tool cache uses a `RouterSnapshot` with pre-cached filtered views per client type for O(1) `list_tools` responses.

```
ServerManager
├── servers: ArcSwap<HashMap<String, Arc<UpstreamServer>>>  (existing)
├── health: DashMap<String, HealthState>                     (NEW)
├── circuit_breakers: DashMap<String, Arc<CircuitBreaker>>   (NEW)
└── semaphores: DashMap<String, Arc<Semaphore>>              (NEW)

ToolRouter
├── ArcSwap<RouterSnapshot>
│   ├── tools_all: Arc<Vec<Tool>>           (full sorted list, for unlimited clients)
│   ├── tools_windsurf: Arc<Vec<Tool>>      (priority-sorted, truncated to 100)
│   ├── tools_copilot: Arc<Vec<Tool>>       (priority-sorted, truncated to 128)
│   └── routes: HashMap<String, String>     (tool name → server name)
└── prefix_delimiter: String
```

New modules:
- `plug-core/src/health.rs` — HealthChecker background task, ping loop, state machine
- `plug-core/src/circuit.rs` — CircuitBreaker state machine (Closed/Open/HalfOpen)
- Extend `plug-core/src/proxy/mod.rs` — RouterSnapshot, tool filtering, token efficiency
- Extend `plug-core/src/config/mod.rs` — new config fields
- Extend `plug-core/src/server/mod.rs` — DashMap containers, reconnection

### Implementation Phases

#### Sub-phase A: Foundation (Health + Concurrency + Config)

**A1. Config extensions** — `plug-core/src/config/mod.rs`

Add to `ServerConfig`:
```rust
/// Max concurrent requests to this server (default: 1 for stdio, 10 for HTTP)
#[serde(default = "default_max_concurrent")]
pub max_concurrent: usize,

/// Health check interval in seconds (default: 60)
#[serde(default = "default_health_interval")]
pub health_check_interval_secs: u64,

/// Enable circuit breaker for this server (default: true)
#[serde(default = "default_true")]
pub circuit_breaker_enabled: bool,
```

Add to `Config`:
```rust
/// Enable client-aware tool filtering (default: true)
#[serde(default = "default_true")]
pub tool_filter_enabled: bool,

/// Max chars for tool descriptions (None = no truncation)
#[serde(default)]
pub tool_description_max_chars: Option<usize>,

/// Tool count threshold to activate search_tools meta-tool (default: 50)
#[serde(default = "default_tool_search_threshold")]
pub tool_search_threshold: usize,

/// Priority tools served first when filtering (tool names)
#[serde(default)]
pub priority_tools: Vec<String>,
```

Add validation in `validate_config()`:
- `max_concurrent == 0` is invalid
- `health_check_interval_secs < 5` is invalid (too aggressive)
- `tool_search_threshold < 10` is invalid
- WARN (not error) if `max_concurrent > 1` for stdio (serial transport)

**A2. Concurrency semaphore** — `plug-core/src/server/mod.rs`

**CORRECTED**: Semaphore lives on `ServerManager` in a `DashMap`, not on `UpstreamServer`. Rationale: `UpstreamServer` gets destroyed on reconnect (ArcSwap swap), which would orphan any outstanding permits. DashMap entry persists across reconnects.

```rust
pub struct ServerManager {
    servers: ArcSwap<HashMap<String, Arc<UpstreamServer>>>,
    pub(crate) health: DashMap<String, HealthState>,
    pub(crate) circuit_breakers: DashMap<String, Arc<CircuitBreaker>>,
    pub(crate) semaphores: DashMap<String, Arc<tokio::sync::Semaphore>>,
}
```

Initialize in `start_server()`:
```rust
self.semaphores.insert(
    name.clone(),
    Arc::new(tokio::sync::Semaphore::new(config.max_concurrent)),
);
```

In `call_tool()` (`proxy/mod.rs`), acquire permit from ServerManager:
```rust
let semaphore = server_manager.semaphores.get(&server_id)
    .ok_or_else(|| ProtocolError::ServerUnavailable { server: server_id.clone() })?;
let permit = semaphore.acquire().await
    .map_err(|_| ProtocolError::ServerUnavailable { server: server_id.clone() })?;
let result = tokio::time::timeout(
    Duration::from_secs(upstream.config.timeout_secs),
    upstream.client.peer().call_tool(params),
).await;
drop(permit);
```

**A3. Health check background task** — `plug-core/src/health.rs`

Follow `session.rs::spawn_cleanup_task()` pattern:
```rust
pub struct HealthChecker;

impl HealthChecker {
    pub fn spawn(
        server_manager: Arc<ServerManager>,
        router: Arc<ToolRouter>,
        cancel: CancellationToken,
        config: &Config,
    ) {
        for (name, sc) in &config.servers {
            if !sc.enabled { continue; }
            let name = name.clone();
            let interval = Duration::from_secs(sc.health_check_interval_secs);
            let mgr = server_manager.clone();
            let router = router.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                // Add jitter: random 0-10s offset
                use rand::Rng;
                let jitter = Duration::from_millis(
                    rand::thread_rng().gen_range(0..10_000)
                );
                tokio::time::sleep(jitter).await;

                let mut interval = tokio::time::interval(interval);
                interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break,
                        _ = interval.tick() => {
                            let changed = health_check_server(&mgr, &name).await;
                            if changed {
                                router.refresh_tools().await;
                            }
                        }
                    }
                }
            });
        }
    }
}
```

Health state machine (stored in `ServerManager.health` DashMap):
- `Healthy` → 3 consecutive failures → `Degraded`
- `Degraded` → 3 more failures → `Failed`
- `Failed` → 1 success → `Degraded`
- `Degraded` → 1 success → `Healthy`

**CORRECTED**: State updates use `DashMap::insert()` — no clone-modify-swap needed. After state changes, call `router.refresh_tools()` to rebuild the RouterSnapshot (which reads from the DashMap). This eliminates torn reads: the snapshot is always consistent.

Ping method: `client.peer().ping().await` (rmcp provides this). If no ping support, use `list_tools()` as a health probe (heavier but universal).

**A4. Tool call timeout** — `plug-core/src/proxy/mod.rs`

Wrap `call_tool` in `tokio::time::timeout()`:
```rust
// Health gate — check DashMap, not UpstreamServer field
let health = server_manager.health.get(&server_id)
    .map(|h| *h)
    .unwrap_or(HealthState::Healthy);
if health == HealthState::Failed {
    return Err(ProtocolError::ServerUnavailable {
        server: server_id.clone(),
    }.into());
}

let result = tokio::time::timeout(
    Duration::from_secs(upstream.config.timeout_secs),
    upstream.client.peer().call_tool(params),
).await;

match result {
    Ok(Ok(response)) => Ok(response),
    Ok(Err(e)) => Err(e.into()),
    Err(_) => Err(ProtocolError::Timeout {
        duration: Duration::from_secs(upstream.config.timeout_secs),
    }.into()),
}
```

#### Sub-phase B: Resilience (Circuit Breakers + Reconnection)

**B1. Circuit breaker** — `plug-core/src/circuit.rs`

**CORRECTED** (per circuit breaker research): Fully lock-free design using `AtomicU64` for timestamps and `tokio::sync::Semaphore` for half-open probes. No `Mutex` anywhere.

```rust
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering::{AcqRel, Acquire, Relaxed, Release}};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

const STATE_CLOSED: u8 = 0;
const STATE_OPEN: u8 = 1;
const STATE_HALF_OPEN: u8 = 2;

static EPOCH: OnceLock<Instant> = OnceLock::new();
fn epoch() -> Instant { *EPOCH.get_or_init(Instant::now) }

pub struct CircuitBreaker {
    state: AtomicU8,
    failure_count: AtomicU32,
    open_since_nanos: AtomicU64,       // u64::MAX = not set
    probe_semaphore: Semaphore,        // 0 permits initially; add on HalfOpen entry
    config: CircuitBreakerConfig,
}

#[derive(Clone)]
pub struct CircuitBreakerConfig {
    pub failure_threshold: u32,     // default: 5
    pub open_duration: Duration,    // default: 30s
    pub probe_count: usize,         // default: 2
}
```

Key design decisions from research:
- **Atomic orderings**: `AcqRel`/`Acquire` for `compare_exchange` on state, `Relaxed` for failure_count `fetch_add`, `Release`/`Acquire` for timestamp read/write
- **Half-open probes**: `Semaphore::new(0)`, `add_permits(N)` on Open→HalfOpen transition, `try_acquire() + permit.forget()` to consume slots permanently
- **Success in Closed resets failure count**: `failure_count.store(0, Relaxed)` — prevents scattered failures from tripping breaker
- **Concurrent trip idempotency**: CAS ensures only one thread transitions Closed→Open

State transitions:
- **Closed**: Track consecutive failures via `AtomicU32`. If `failures >= threshold` → CAS to Open, store timestamp
- **Open**: Reject immediately. Check `open_since_nanos` — if `open_duration` elapsed → CAS to HalfOpen, add probe permits
- **HalfOpen**: `try_acquire()` probe permit. If success response → CAS to Closed, reset counts. If failure → CAS back to Open

Lives on `ServerManager.circuit_breakers` DashMap (persists across reconnects):
```rust
// In call_tool():
let cb = server_manager.circuit_breakers.get(&server_id);
if let Some(cb) = &cb {
    cb.call_allowed()?;
}
// ... do the call ...
match result {
    Ok(_) => { if let Some(cb) = &cb { cb.on_success(); } }
    Err(_) => { if let Some(cb) = &cb { cb.on_failure(); } }
}
```

Testing: Use `#[tokio::test(start_paused = true)]` + `tokio::time::advance()` for deterministic time-dependent tests. Test concurrent trip (100 tasks failing simultaneously) to verify CAS idempotency.

**B2. Reconnection with backoff** — `plug-core/src/server/mod.rs`

**CORRECTED** (per backon research): backon v1.6 API requires explicit `.sleep(tokio::time::sleep)`.

```toml
# Cargo.toml workspace
backon = "1.6"
```

When health check detects a server as `Failed`, attempt reconnection:
```rust
use backon::{ExponentialBuilder, Retryable};

let reconnect = || async { ServerManager::start_server(&name, &config).await };
let result = reconnect
    .retry(ExponentialBuilder::default()
        .with_min_delay(Duration::from_secs(1))
        .with_max_delay(Duration::from_secs(60))
        .with_max_times(5))
    .sleep(tokio::time::sleep)  // REQUIRED in backon v1.6
    .when(|e| {
        tracing::warn!(server = %name, error = %e, "reconnection attempt failed");
        true // retry all errors
    })
    .notify(|err: &anyhow::Error, dur: Duration| {
        tracing::info!(server = %name, error = %err, delay = ?dur, "retrying reconnection");
    })
    .await;
```

On success:
1. Swap the new `UpstreamServer` into the servers map
2. Reset the circuit breaker: `cb.reset()` (store Closed state, clear failure count and timestamp)
3. **Call `router.refresh_tools().await`** — new server may have different tools

#### Sub-phase C: Efficiency (Filtering + Token + Search)

**C1. Store client_type in session** — `plug-core/src/http/session.rs` + `plug-core/src/proxy/mod.rs`

HTTP path — add to `SessionState`:
```rust
struct SessionState {
    last_activity: Instant,
    sse_sender: Option<mpsc::Sender<SseMessage>>,
    client_type: ClientType,  // NEW
}
```

Stdio path — **CORRECTED**: use `RwLock<ClientType>` not `OnceLock`:
```rust
pub struct ProxyHandler {
    router: Arc<ToolRouter>,
    client_type: std::sync::RwLock<ClientType>,  // RwLock, not OnceLock
}
```

Rationale: `OnceLock::set()` silently discards the second call, which is wrong if a client re-initializes (Phase 4 daemon mode). `RwLock` allows updating.

```rust
// In initialize():
let ct = detect_client(&request.client_info.name);
*self.client_type.write().unwrap() = ct;
```

**C2. Client-aware tool filtering** — `plug-core/src/proxy/mod.rs`

**CORRECTED** (per performance review): Pre-cache filtered views at `refresh_tools()` time, not per-request. Only 3 distinct limit buckets exist (Windsurf: 100, Copilot: 128, unlimited).

```rust
pub(crate) struct RouterSnapshot {
    pub tools_all: Arc<Vec<Tool>>,          // full sorted list
    pub tools_windsurf: Arc<Vec<Tool>>,     // priority-sorted, truncated to 100
    pub tools_copilot: Arc<Vec<Tool>>,      // priority-sorted, truncated to 128
    pub routes: HashMap<String, String>,    // tool name → server name
}
```

In `refresh_tools()`, build all three views in a single pass:
```rust
// 1. Build full sorted list (priority_tools first, then alphabetical)
let mut all_tools = /* collect from healthy servers */;
all_tools.sort_unstable_by(|a, b| priority_sort(a, b, &config.priority_tools));

// 2. Pre-cache filtered views
let tools_windsurf = Arc::new(all_tools.iter().take(100).cloned().collect());
let tools_copilot = Arc::new(all_tools.iter().take(128).cloned().collect());
let tools_all = Arc::new(all_tools);
```

`list_tools_for_client()` becomes a single `Arc::clone()` — effectively free:
```rust
pub fn list_tools_for_client(&self, client_type: ClientType) -> Arc<Vec<Tool>> {
    let snapshot = self.cache.load();
    match client_type.tool_limit() {
        Some(100) => Arc::clone(&snapshot.tools_windsurf),
        Some(128) => Arc::clone(&snapshot.tools_copilot),
        _ => Arc::clone(&snapshot.tools_all),
    }
}
```

**C3. Token efficiency** — `plug-core/src/proxy/mod.rs`

**CONFIRMED** (per rmcp-tool research): `Tool` derives `Clone` and `#[non_exhaustive]` only blocks struct literals, not field mutation on owned values. Stripping pattern:

```rust
fn strip_optional_fields(tool: &mut Tool, max_desc_chars: Option<usize>) {
    tool.title = None;
    tool.output_schema = None;
    tool.annotations = None;
    tool.icons = None;
    if let Some(max) = max_desc_chars {
        if let Some(ref desc) = tool.description {
            if desc.len() > max {
                let truncated = desc.chars().take(max).collect::<String>();
                tool.description = Some(std::borrow::Cow::Owned(truncated));
            }
        }
    }
}
```

All optional fields use `#[serde(skip_serializing_if = "Option::is_none")]` — setting to `None` removes them from the wire. `inputSchema` is REQUIRED per MCP spec (ADR-003) — never strip it.

Apply during `refresh_tools()` when building the RouterSnapshot.

**C4. Tool search meta-tool** — `plug-core/src/proxy/mod.rs`

**CONFIRMED** (per rmcp-tool research): Use `Tool::new()` to build the meta-tool:
```rust
fn build_search_tools_meta_tool() -> Tool {
    Tool::new(
        Cow::Borrowed("plug__search_tools"),
        Cow::Borrowed("Search for tools by name or description. Returns matching tools with full schemas."),
        Arc::new(serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query for tool name or description" }
            },
            "required": ["query"]
        }).as_object().unwrap().clone()),
    )
}
```

Register when total tools > threshold. Search scans `tools_all` directly from the ArcSwap guard — no allocation needed. Serialize matches directly from `&Tool` references. Linear scan at 200 tools takes ~20-50μs — no index needed.

In `call_tool()`, intercept calls to `plug__search_tools` before routing to upstreams.

## System-Wide Impact

### Interaction Graph

1. Health check ping → `client.peer().ping()` → rmcp transport → upstream server
2. Health state change → `DashMap::insert()` → `router.refresh_tools()` → rebuilds RouterSnapshot → next `list_tools` response reflects new health
3. Circuit breaker open → `call_tool()` returns `ServerUnavailable` immediately → client gets error
4. Client `initialize()` → `detect_client()` → store in session/RwLock → `list_tools()` selects pre-cached view → filtered response
5. Tool search call → `call_tool("plug__search_tools")` → intercepted in proxy → scans tool cache → returns results

### Error & Failure Propagation

- **Upstream timeout**: `tokio::time::timeout` wraps `call_tool` → returns `ProtocolError::Timeout` → circuit breaker `on_failure()` → if `failure_count >= threshold` → CAS to Open → subsequent calls get `ServerUnavailable`
- **Health check failure**: ping fails → `DashMap::insert(Degraded/Failed)` → `refresh_tools()` rebuilds snapshot excluding Failed servers → clients don't see those tools
- **Reconnection failure**: `backon` retries with exponential backoff (1s-60s, max 5) → if all fail, server stays `Failed` → next health check cycle tries again
- **Reconnection success**: swap new UpstreamServer → reset circuit breaker → `refresh_tools()` rebuilds snapshot with new tools

### State Lifecycle Risks

- **Torn reads**: ELIMINATED — RouterSnapshot is an atomic unit in ArcSwap; health state in DashMap is read at `refresh_tools()` time and baked into the snapshot
- **Stale permits**: Semaphore permits dropped after `call_tool` returns (both success and error paths). Semaphore persists in DashMap across reconnects.
- **Orphaned health tasks**: `CancellationToken` ensures all background tasks stop on shutdown. **ENFORCE cancellation-first shutdown order**: `cancel.cancel()` BEFORE `server_manager.shutdown_all()`
- **Circuit breaker persistence**: Lives in DashMap, survives UpstreamServer reconnects. `reset()` called explicitly on successful reconnection.
- **Orphaned probe permits**: `permit.forget()` consumes permits permanently. New `add_permits()` on each Open→HalfOpen transition.

### Integration Test Scenarios

1. **Server crash + recovery**: Start mock server → make tool calls → kill server → verify circuit opens → restart server → verify circuit closes → verify tools reappear
2. **Concurrent tool calls with semaphore**: Start mock server with 100ms latency → fire 10 concurrent `call_tool` → verify max `max_concurrent` run simultaneously
3. **Windsurf tool filtering**: Initialize with `clientInfo.name = "windsurf-client"` → configure 150 tools → verify `list_tools` returns exactly 100
4. **Health check state transitions**: Start mock server → respond to pings → stop responding → verify Healthy → Degraded → Failed transitions → resume pings → verify recovery
5. **Tool search**: Configure > 50 tools → verify `plug__search_tools` appears in list → call it with query → verify filtered results

## Acceptance Criteria

### Functional Requirements

- [ ] Health checks ping each upstream every 60s (configurable) with jitter
- [ ] Health state machine: Healthy → Degraded (3 failures) → Failed (3 more) → recovery on success
- [ ] Failed servers' tools removed from `list_tools` response automatically
- [ ] Circuit breaker opens after 5 consecutive failures, rejects for 30s, allows 2 half-open probes
- [ ] `call_tool` respects `timeout_secs` config, returns `ProtocolError::Timeout` on expiry
- [ ] Per-server concurrency limited by `tokio::sync::Semaphore` (default 1 for stdio)
- [ ] Reconnection with exponential backoff via `backon` (1s-60s, max 5 attempts)
- [ ] `client_type` stored during `initialize()` for both stdio and HTTP paths
- [ ] Windsurf gets max 100 tools, VS Code Copilot gets max 128
- [ ] Priority tools sorted first when filtering, pre-cached at refresh time
- [ ] Optional fields (`title`, `outputSchema`, `annotations`, `icons`) stripped from tools/list
- [ ] Tool descriptions truncated to `tool_description_max_chars` when configured
- [ ] `plug__search_tools` meta-tool registered when total tools > threshold
- [ ] Search returns top 10 matches by name/description/server
- [ ] `refresh_tools()` called after successful reconnection

### Non-Functional Requirements

- [ ] Tool call overhead < 1ms for cached routes (semaphore acquire + circuit check + timeout wrap ~500ns)
- [ ] `list_tools_for_client()` is O(1) — single `Arc::clone()` from pre-cached view
- [ ] Health check does not block request path (background task + DashMap)
- [ ] All 70+ existing tests still pass
- [ ] `cargo clippy -- -D warnings` clean
- [ ] `#![forbid(unsafe_code)]` maintained

### Quality Gates

- [ ] Unit tests for circuit breaker state machine (all transitions, concurrent trip idempotency)
- [ ] Unit tests with `start_paused = true` for time-dependent transitions (Open→HalfOpen)
- [ ] Unit tests for health state machine (all transitions)
- [ ] Unit tests for tool filtering (each client type, edge cases, pre-cached views)
- [ ] Unit tests for tool search (name match, description match, empty results)
- [ ] Unit tests for token stripping (all optional fields removed, inputSchema preserved)
- [ ] Integration test: server crash + circuit breaker + recovery
- [ ] Integration test: client-aware filtering with mock client

## Dependencies & Prerequisites

**Crate additions** (workspace `Cargo.toml`):
- `backon = "1.6"` — exponential backoff (replaces unmaintained `backoff`)
- `rand = "0.8"` — jitter for health check intervals

**No new crates needed for**:
- Circuit breaker: DIY with `AtomicU8` + `AtomicU32` + `AtomicU64` + `tokio::sync::Semaphore`
- Concurrency: `tokio::sync::Semaphore` (already in tokio "full")
- Tool filtering: pure Rust logic in proxy module
- DashMap: already in dependencies (used by SessionManager)

**Locked decisions (do not revisit)**:
- ADR-003: `inputSchema` is REQUIRED, never strip it
- ADR-005: Cursor has no tool limit (Dynamic Context Discovery, Jan 2026)
- ADR-006: Use `backon` not `backoff`, `Semaphore` not `flow-guard`
- ADR-007: Exact-match client detection, fuzzy fallback

## Risk Analysis & Mitigation

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| rmcp `Tool` is non-exhaustive, can't strip fields | **RESOLVED** | — | rmcp Tool derives Clone; field mutation on owned values is permitted; `skip_serializing_if = "Option::is_none"` removes None fields from wire |
| `tower-resilience` is immature (pre-1.0) | **RESOLVED** | — | DIY lock-free circuit breaker with AtomicU8/U32/U64 + Semaphore. Complete struct skeleton validated by research |
| Health check ping not supported by all servers | Medium | Low | Fallback to `list_tools()` as health probe |
| Semaphore deadlock if not dropped on error path | Low | High | Use `drop(permit)` explicitly; semaphore on ServerManager DashMap survives reconnects |
| Gemini CLI 60s timeout during startup | High | High | Pre-cache tools at startup, return cached immediately |
| `max_concurrent > 1` misleading for stdio | Medium | Low | Warn in config validation (stdio is serial) |
| Stale tools after reconnect | Medium | Medium | **RESOLVED**: Call `refresh_tools()` after successful reconnect |

## Files to Create/Modify

### New Files
- `plug-core/src/health.rs` — Health checker background task + state machine
- `plug-core/src/circuit.rs` — Lock-free circuit breaker with AtomicU64 timestamps

### Modified Files
- `plug-core/src/lib.rs` — add `pub mod health; pub mod circuit;`
- `plug-core/src/config/mod.rs` — new config fields + validation
- `plug-core/src/server/mod.rs` — add DashMap containers (health, circuit_breakers, semaphores), reconnection logic
- `plug-core/src/proxy/mod.rs` — RouterSnapshot, pre-cached filtering, token stripping, tool search, call_tool timeout + circuit + health gate
- `plug-core/src/types.rs` — HealthState enum with failure counter
- `plug-core/src/http/session.rs` — add `client_type` to SessionState
- `plug-core/src/http/server.rs` — store client_type during initialize, pass to list_tools
- `plug/src/main.rs` — spawn health checker in `cmd_connect` and `cmd_serve`
- `Cargo.toml` (workspace) — add `backon`, `rand`
- `plug-core/Cargo.toml` — add `backon`, `rand` deps

## Sources & References

### Internal References
- Phase 2 patterns: `docs/solutions/integration-issues/mcp-multiplexer-http-transport-phase2.md`
- rmcp SDK gotchas: `docs/solutions/integration-issues/rmcp-sdk-integration-patterns-plug-20260303.md`
- Architecture decisions: `docs/DECISIONS.md` (ADR-003, ADR-005, ADR-006, ADR-007)
- Crate validation: `docs/CRATE-STACK.md`
- Client validation: `docs/research/client-validation.md`
- Implementation plan: `docs/PLAN.md:133-184`

### Existing Code
- ServerManager: `plug-core/src/server/mod.rs:32-34`
- ToolRouter: `plug-core/src/proxy/mod.rs:22-29`
- ServerHealth enum: `plug-core/src/types.rs:71-79`
- ClientType::tool_limit(): `plug-core/src/types.rs:43-50`
- detect_client(): `plug-core/src/client_detect.rs`
- Session cleanup pattern: `plug-core/src/http/session.rs:99-125`
- ProxyHandler initialize: `plug-core/src/proxy/mod.rs:190-207`

### Research References
- [Rust Atomics and Locks — Memory Ordering](https://marabos.nl/atomics/memory-ordering.html)
- [tokio::sync::Semaphore](https://docs.rs/tokio/latest/tokio/sync/struct.Semaphore.html)
- [backon v1.6 docs](https://docs.rs/backon/1.6.0/backon/)
- [tokio test-util](https://docs.rs/tokio/latest/tokio/attr.test.html)
- [linkerd2-proxy circuit breaking](https://linkerd.io/2-edge/reference/circuit-breaking/)

### Related Work
- PR #1: Phase 1 stdio proxy
- PR #2: Phase 2 HTTP transport
