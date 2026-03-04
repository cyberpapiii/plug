---
module: plug-core
date: 2026-03-03
problem_type: integration_issues
component: rust_crate
symptoms:
  - "E0639: cannot construct non-exhaustive struct with struct literal syntax"
  - "rmcp::error::ErrorData — private module path compilation failure"
  - "Torn reads between list_tools and call_tool due to non-atomic cache swap"
  - "RwLock held across async .await blocking concurrent tasks"
  - "Figment env var PLUG_LOG_LEVEL parsed as nested 'log.level' instead of 'log_level'"
root_cause: unfamiliar_sdk_api
severity: high
tags:
  - rmcp
  - mcp
  - arc-swap
  - figment
  - async-rust
  - non-exhaustive
  - proxy-architecture
---

# rmcp SDK Integration Patterns for MCP Proxy Development

## Problem Statement

Building an MCP (Model Context Protocol) multiplexer proxy in Rust using rmcp 1.0.0 SDK. Multiple non-obvious API patterns and architectural pitfalls were discovered during implementation that blocked progress until resolved.

## Investigation Steps

1. Attempted direct struct literal construction for rmcp types — failed with E0639
2. Tried importing `rmcp::error::ErrorData` — failed due to private module
3. Used separate `ArcSwap` instances for tool routes and tool definitions — caused torn reads
4. Held `OwnedRwLockReadGuard` across upstream RPC `.await` — blocked concurrent tool calls
5. Used `.split("_")` for Figment env var nesting — broke underscore field names

## Solutions

### 1. rmcp Non-Exhaustive Struct Construction

**Problem:** rmcp 1.0.0 marks key structs as `#[non_exhaustive]`, preventing direct construction.

```rust
// WRONG — E0639: cannot construct non-exhaustive struct
let result = InitializeResult {
    capabilities: caps,
    server_info: info,
    ..Default::default()  // doesn't help
};
```

```rust
// CORRECT — use builder methods
let result = InitializeResult::new(capabilities)
    .with_server_info(Implementation::new("plug", env!("CARGO_PKG_VERSION")));

// For tool call results:
let result = CallToolResult::success(vec![Content::text(response_text)]);
```

**Key Insight:** When a Rust crate uses `#[non_exhaustive]`, always search the source for `fn new`, `fn builder`, `fn with_*`, or `fn success`/`fn error` factory methods. The crate author intentionally prevents direct construction.

### 2. rmcp Module Re-exports

**Problem:** Internal module paths are private; the crate re-exports types at the root.

```rust
// WRONG — private module
use rmcp::error::ErrorData;
use rmcp::transport::io::stdio_transport;
```

```rust
// CORRECT — use re-exported paths
use rmcp::ErrorData as McpError;
let transport = rmcp::transport::io::stdio();  // returns (Stdin, Stdout) tuple
```

**Key Insight:** Always check the crate's `lib.rs` for `pub use` re-exports. The public API surface may differ significantly from the internal module structure. Run `cargo doc --open` for the authoritative API.

### 3. Atomic Tool Cache with ArcSwap

**Problem:** Two separate `ArcSwap::store()` calls for related data creates a window where one is updated but the other isn't.

```rust
// WRONG — torn reads possible between these two stores
self.merged_tools.store(Arc::new(tools));
self.tool_routes.store(Arc::new(routes));
// A concurrent list_tools call here sees new tools but old routes
```

```rust
// CORRECT — combine into single struct, swap atomically
struct ToolCache {
    routes: HashMap<String, String>,
    tools: Vec<Tool>,
}

// Single atomic swap
self.cache.store(Arc::new(ToolCache { routes, tools }));
```

**Key Insight:** When multiple pieces of data must be consistent with each other, group them into a single struct behind one `ArcSwap`. This is the "snapshot" pattern — readers always see a consistent view.

### 4. RwLock Across Async Boundaries

**Problem:** Holding a lock guard across an `.await` point blocks other tasks that need the lock.

```rust
// WRONG — guard held during entire RPC call
let guard = self.server_manager.get_server(server_id).await?;
let upstream = &guard[server_id];
let result = upstream.client.peer()
    .call_tool(params)
    .await?;  // Lock held here — blocks shutdown, refresh, other calls
```

```rust
// CORRECT — clone what you need, drop the guard, then await
let peer = {
    let guard = self.server_manager.get_server(&server_id).await?;
    let upstream = &guard[&server_id];
    upstream.client.peer().clone()  // Clone the peer
};
// Lock is released here

let result: CallToolResult = peer
    .call_tool(params)
    .await?;  // No lock held — other tasks can proceed
```

**Key Insight:** In async Rust, treat lock guards like hot potatoes — hold them for the minimum scope, clone/extract what you need, and drop before any `.await`. The `Peer` type in rmcp is cheaply cloneable (it wraps an `Arc`).

### 5. Figment Env Var Split Delimiter

**Problem:** Using single underscore as the nesting delimiter conflicts with field names that contain underscores.

```rust
// WRONG — PLUG_LOG_LEVEL is parsed as config.log.level (nested)
.merge(Env::prefixed("PLUG_").split("_"))
```

```rust
// CORRECT — double underscore for nesting, single underscore preserved in field names
.merge(Env::prefixed("PLUG_").split("__"))
// PLUG_LOG_LEVEL → config.log_level (flat field)
// PLUG_SERVERS__MYSERVER__TIMEOUT → config.servers.myserver.timeout (nested)
```

**Key Insight:** When using Figment with env vars, always use a multi-character delimiter (like `__`) for nesting if your config fields contain underscores. This is a common gotcha documented nowhere in Figment's examples.

## Prevention Strategies

### For rmcp API Issues
- **Before coding:** Run `cargo doc -p rmcp --open` and browse the generated docs
- **Pattern:** Search source for `pub fn new` on any type you need to construct
- **Test:** Write a minimal compilation test for each rmcp type you use

### For Atomic Data Consistency
- **Rule:** If two pieces of data must be consistent, they go in one `ArcSwap`
- **Detection:** Code review flag — multiple `.store()` calls on related `ArcSwap` instances
- **Clippy:** No built-in lint, but a custom lint could catch this pattern

### For Async Lock Safety
- **Rule:** Never hold a lock guard across `.await` — always clone-and-drop
- **Detection:** `clippy::await_holding_lock` catches `MutexGuard` but not `OwnedRwLockReadGuard`
- **Pattern:** Use a block scope `let data = { let guard = ...; guard.clone_data() };`

### For Figment Configuration
- **Rule:** Always use `__` (double underscore) as the split delimiter
- **Test:** Write a unit test that sets an env var for a field with underscores and verifies it resolves correctly
- **Documentation:** Document the env var naming convention in your project README

## Related Documentation

- [docs/research/rmcp-feasibility.md](../research/rmcp-feasibility.md) — Initial rmcp SDK research
- [docs/research/crate-validation.md](../research/crate-validation.md) — Crate version validation
- [docs/ARCHITECTURE.md](../ARCHITECTURE.md) — System architecture
- [docs/CRATE-STACK.md](../CRATE-STACK.md) — Dependency decisions
- PR: https://github.com/cyberpapiii/plug/pull/1

## Environment

- **Rust edition:** 2024
- **rmcp version:** 1.0.0
- **figment version:** 0.10
- **arc-swap version:** 1.7
- **tokio version:** 1.x
- **OS:** macOS Darwin 25.4.0
