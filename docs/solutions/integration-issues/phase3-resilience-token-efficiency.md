---
title: "Phase 3 Resilience & Token Efficiency — Circuit Breakers, Health Checks, and rmcp API Patterns"
category: integration-issues
tags:
  - circuit-breaker
  - rmcp-sdk
  - concurrent-state-management
  - token-efficiency
  - health-checks
  - lock-free
  - utf8
  - secret-redaction
module: "plug-core (circuit.rs, health.rs, proxy/mod.rs, server/mod.rs, types.rs, config/mod.rs)"
note: "Historical solution record from the Phase 3 implementation window. It includes rmcp 1.0.0-era details and should not be treated as current-state truth."
symptom: |
  Multiple integration bugs surfaced during Phase 3 resilience implementation:
  1. Circuit breaker semaphore permits accumulate across half-open cycles
  2. rmcp 1.0.0 SDK lacks Peer::ping() for health probing
  3. rmcp CallToolResult::text() doesn't exist — requires success(vec![Content::text(...)])
  4. expand_env_vars advances by 1 byte instead of char.len_utf8(), corrupting multi-byte UTF-8
  5. auth_token visible in Debug output, leaking credentials in logs
  6. Magic numbers (100, 128) for client tool limits create fragile coupling
root_cause: |
  Primary: Semaphore initialized with 0 permits; add_permits() called on Open→HalfOpen
  without draining leftovers from previous cycles. Secondary: rmcp 1.0.0 API surface
  differs from assumptions — no ping(), no text() convenience method. Tertiary: byte-level
  iteration in expand_env_vars doesn't account for multi-byte UTF-8 codepoints.
date: "2026-03-03"
severity: high
status: resolved
commits:
  - "f08b08b: feat: implement Phase 3 resilience and token efficiency"
  - "71e0657: fix: address code review — circuit breaker permits, UTF-8, auth redaction"
---

# Phase 3 Resilience & Token Efficiency

## Problem Summary

Phase 3 of the plug MCP multiplexer added circuit breakers, health checks, concurrency limiting, client-aware tool filtering, token efficiency, and a search_tools meta-tool. During implementation and code review, 7 distinct issues were discovered and fixed across the resilience and efficiency subsystems.

## Solutions

### 1. Lock-Free Circuit Breaker Pattern

All state is represented with three atomics — no mutex on the hot path:

```rust
// plug-core/src/circuit.rs
pub struct CircuitBreaker {
    state: AtomicU8,           // STATE_CLOSED=0, STATE_OPEN=1, STATE_HALF_OPEN=2
    failure_count: AtomicU32,
    /// Nanoseconds since EPOCH when the circuit was opened.
    open_since_nanos: AtomicU64,
    /// Semaphore starts with 0 permits; permits are added on Open→HalfOpen.
    probe_semaphore: Semaphore,
    config: CircuitBreakerConfig,
}
```

Timestamps use a process-wide monotonic epoch (`OnceLock<Instant>`) converted to epoch-relative nanoseconds so they fit in a `u64`:

```rust
static EPOCH: OnceLock<Instant> = OnceLock::new();

fn nanos_since_epoch() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_nanos() as u64
}
```

The Open→HalfOpen transition uses CAS. The critical permit drain happens immediately after:

```rust
if self.state.compare_exchange(
    STATE_OPEN, STATE_HALF_OPEN,
    Ordering::AcqRel, Ordering::Acquire,
).is_ok() {
    // Drain leftover permits from previous half-open cycle
    while self.probe_semaphore.try_acquire().is_ok() {}
    self.probe_semaphore.add_permits(self.config.probe_count);
}
```

Probe acquisition permanently consumes the permit using `forget()`:

```rust
fn try_acquire_probe(&self) -> Result<(), CircuitBreakerError> {
    match self.probe_semaphore.try_acquire() {
        Ok(permit) => {
            permit.forget(); // consume permanently
            Ok(())
        }
        Err(_) => Err(CircuitBreakerError),
    }
}
```

### 2. DashMap vs ArcSwap Split

Mutable per-server state (health, circuit, semaphore) goes in `DashMap`. Immutable snapshots (tool cache) stay in `ArcSwap`.

```rust
// plug-core/src/server/mod.rs
pub struct ServerManager {
    // ArcSwap: immutable snapshot, wait-free reads for HTTP concurrency.
    servers: ArcSwap<HashMap<String, Arc<UpstreamServer>>>,

    // DashMap: per-server mutable state, updated independently at high frequency.
    pub(crate) health: DashMap<String, HealthState>,
    pub(crate) circuit_breakers: DashMap<String, Arc<CircuitBreaker>>,
    pub(crate) semaphores: DashMap<String, Arc<tokio::sync::Semaphore>>,
}
```

**Rule**: `ArcSwap` when the whole collection is replaced atomically (server map, tool cache). `DashMap` when individual entries are mutated independently (health counters, failure counts).

### 3. RouterSnapshot with Pre-Cached Filtered Views

Pre-compute filtered tool lists at `refresh_tools()` time for O(1) client-aware filtering:

```rust
// plug-core/src/proxy/mod.rs
pub(crate) struct RouterSnapshot {
    pub tools_all: Arc<Vec<Tool>>,
    pub tools_windsurf: Arc<Vec<Tool>>,   // truncated to 100
    pub tools_copilot: Arc<Vec<Tool>>,    // truncated to 128
    pub routes: HashMap<String, String>,
}
```

`list_tools_for_client()` is a single `Arc::clone` — O(1):

```rust
pub fn list_tools_for_client(&self, client_type: ClientType) -> Arc<Vec<Tool>> {
    let snapshot = self.cache.load();
    match client_type {
        ClientType::Windsurf    => Arc::clone(&snapshot.tools_windsurf),
        ClientType::VSCodeCopilot => Arc::clone(&snapshot.tools_copilot),
        _                       => Arc::clone(&snapshot.tools_all),
    }
}
```

### 4. rmcp 1.0.0 API Workarounds

**`CallToolResult::success()` instead of `text()`:**

```rust
// WRONG: CallToolResult::text("message")  — doesn't exist in rmcp 1.0.0
// CORRECT:
Ok(CallToolResult::success(vec![Content::text("Please provide a search query.")]))
```

**`list_all_tools()` instead of `ping()`:**

```rust
// WRONG: upstream.client.peer().ping()  — not available on Peer<RoleClient>
// CORRECT:
upstream.client.peer().list_all_tools().await
```

**`Tool::new()` for non-exhaustive structs:**

```rust
Tool::new(
    Cow::Borrowed("plug__search_tools"),
    Cow::Borrowed("Search for tools by name or description."),
    Arc::new(serde_json::json!({"type": "object", ...}).as_object().unwrap().clone()),
)
```

### 5. SecretString for Auth Redaction

Newtype that overrides `Debug` to print `[REDACTED]` while `Display` shows the real value:

```rust
// plug-core/src/types.rs
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
```

`#[serde(transparent)]` means config files work without changes. `Display` is used for `format!("Bearer {token}")` in HTTP auth headers.

### 6. UTF-8 Safe Env Var Expansion

Advance by `char.len_utf8()` instead of 1 byte when copying non-variable characters:

```rust
// plug-core/src/config/expand.rs
// WRONG:
// result.push(input[i..].chars().next().unwrap());
// i += 1;  // breaks on multi-byte chars like ñ (2 bytes) or 世 (3 bytes)

// CORRECT:
let ch = input[i..].chars().next().unwrap();
result.push(ch);
i += ch.len_utf8();
```

### 7. Semaphore Permit Leak Fix

**Problem**: `add_permits()` is cumulative. If the circuit cycles Open→HalfOpen multiple times without consuming all permits, each cycle adds `probe_count` more permits on top of leftovers. A 2-probe circuit that re-opens 10 times accumulates up to 20 permits.

**Fix**: Drain before adding:

```rust
if self.state.compare_exchange(STATE_OPEN, STATE_HALF_OPEN, ...).is_ok() {
    while self.probe_semaphore.try_acquire().is_ok() {}  // drain leftovers
    self.probe_semaphore.add_permits(self.config.probe_count);
}
```

## Prevention Strategies

### rmcp API Misuse
- **Anti-pattern**: Assuming convenience methods exist (`.text()`, `.ping()`)
- **Best practice**: Check rmcp source/docs before every API call
- **Rule**: Always validate rmcp API surface against the source crate — assumptions lead to compilation failures

### Semaphore Permit Accumulation
- **Anti-pattern**: `add_permits()` without draining in cyclic state machines
- **Best practice**: `while try_acquire().is_ok() {}` before `add_permits()` in every cycle
- **Rule**: In cyclic semaphore patterns, always drain stale permits before adding fresh ones

### UTF-8 Byte Iteration
- **Anti-pattern**: `i += 1` after extracting a char from a byte-indexed string
- **Best practice**: `i += ch.len_utf8()` or use `chars()` iterator
- **Rule**: Never advance by 1 byte per character — use `len_utf8()` for multi-byte safety

### Secret Leakage in Debug
- **Anti-pattern**: `#[derive(Debug)]` on structs with sensitive fields
- **Best practice**: `SecretString` wrapper or custom `Debug` impl that redacts
- **Rule**: Never derive Debug on structs with secrets — use SecretString or custom impl

### Magic Number Coupling
- **Anti-pattern**: `match client_type.tool_limit() { Some(100) => ... }`
- **Best practice**: `match client_type { ClientType::Windsurf => ... }`
- **Rule**: Match on semantic types, not their computed properties

### DashMap Guard Lifetime
- **Anti-pattern**: Holding DashMap write guards across `.await` points
- **Best practice**: Minimize guard scope, drop before any async boundary
- **Rule**: Never hold DashMap/Mutex guards across `.await` — deadlock risk

## Architecture: Tool Call Pipeline

```
Input: prefixed_tool_name, arguments
  │
  ├─ [Intercept] search_tools meta-tool → handle locally
  │
  ├─ [Route Lookup] find server_id from routing table
  │
  ├─ [Health Gate] reject if server health == Failed
  │
  ├─ [Circuit Breaker] reject if call_allowed() fails
  │
  ├─ [Semaphore] acquire owned permit (blocks if at capacity)
  │
  ├─ [Timeout] wrap upstream call in tokio::time::timeout
  │
  ├─ [Record] call on_success() or on_failure() on circuit breaker
  │
  └─ Output: CallToolResult or McpError
```

## Related Documents

- [Phase 2 HTTP Transport Learnings](mcp-multiplexer-http-transport-phase2.md) — ArcSwap, ToolRouter extraction, CancellationToken patterns
- [rmcp SDK Integration Patterns](rmcp-sdk-integration-patterns-plug-20260303.md) — Non-exhaustive structs, RwLock vs OnceLock, Figment config
- [Phase 3 Plan](../../plans/2026-03-03-feat-phase-3-resilience-token-efficiency-plan.md) — Full technical spec
- [Architecture Decisions](../DECISIONS.md) — ADR-005 (client limits), ADR-006 (crate choices)
- [Risk Register](../RISKS.md) — R4 (server compatibility), R7 (Gemini timeout)

## Test Coverage

- **Circuit breaker**: 10 tests (concurrent trip idempotency, probe count limits, state transitions)
- **Health state machine**: 4 tests (degraded/failed transitions, recovery, counter reset)
- **Tool filtering**: list_tools_for_client with 150 mock tools
- **search_tools**: meta-tool search and result formatting
- **Integration**: 13 tests covering ProxyHandler, config, client detection
- **Total**: 88 tests passing, clippy clean
