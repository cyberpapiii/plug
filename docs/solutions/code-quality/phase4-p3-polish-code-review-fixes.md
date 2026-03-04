---
title: "Phase 4 P3 Polish: TUI dirty flag, RouterConfig DRY, and tracing split"
date: 2026-03-04
category: code-quality
tags: [performance, simplicity, architecture, code-review, tui, daemon, tracing]
severity: p3
component: tui, proxy, daemon
pr: 5
branch: fix/p3-polish-018-020
symptoms: |
  1. TUI redraws unnecessarily on ConfigReloaded/CircuitBreakerTripped events
  2. Navigation keys set dirty flag even when selection doesn't change
  3. RouterConfig construction duplicated across main.rs and engine.rs (5 fields)
  4. Daemon file logging blocked by early stderr subscriber initialization
root_cause: |
  #018: navigate_focused_panel() had no return value; callers always set dirty.
        ConfigReloaded/CircuitBreakerTripped unconditionally set dirty despite no state change.
  #019: Manual field-by-field RouterConfig in two places with no shared From impl.
  #020: main() initialized stderr tracing before command dispatch, preventing daemon's
        setup_file_logging() from installing its own subscriber (single-init constraint).
---

# Phase 4 P3 Polish: Code Review Fixes

Three P3 polish items discovered during the Phase 4 code review, resolved in PR #5.

## Fix #018: Dirty Flag on Noop Events

### Root Cause

The TUI `App` set `self.dirty = true` unconditionally on every navigation keypress and for events that don't change visible state (`ConfigReloaded`, `CircuitBreakerTripped`). Each spurious dirty flag triggers a full terminal redraw cycle — layout rebuild, widget iteration, terminal flush.

### Solution

`navigate_focused_panel()` now returns `bool` indicating whether selection changed:

```rust
// app.rs — navigation returns change indicator
fn navigate_focused_panel(&mut self, direction: i32) -> bool {
    let mut changed = false;
    match self.focused_panel {
        0 => {
            let len = self.servers.len();
            if len > 0 {
                let i = self.server_state.selected().unwrap_or(0) as i32;
                let next = (i + direction).clamp(0, len as i32 - 1) as usize;
                if next != i as usize {
                    self.server_state.select(Some(next));
                    changed = true;
                }
            }
        }
        // ... same pattern for panels 1, 2
        _ => {}
    }
    changed
}

// Caller only sets dirty when selection moved:
KeyCode::Down | KeyCode::Char('j') => {
    if self.navigate_focused_panel(1) {
        self.dirty = true;
    }
}
```

Events with no visual effect are now no-ops:

```rust
EngineEvent::ConfigReloaded | EngineEvent::CircuitBreakerTripped { .. } => {}
```

### Why This Works

The dirty flag is a contract with the render loop. Only visual state changes warrant a redraw. `ConfigReloaded` doesn't update any `App` field. `CircuitBreakerTripped` is followed by a separate `ServerHealthChanged` event from health checks, which does update state and set dirty.

---

## Fix #019: DRY RouterConfig Construction

### Root Cause

`RouterConfig` was manually constructed field-by-field in `Engine::new()` (engine.rs) and `cmd_tool_list()` (main.rs) — identical 5-field struct literals in two crates. Adding a field meant updating both.

### Solution

Single `From<&Config>` impl alongside the `RouterConfig` definition:

```rust
// proxy/mod.rs — single source of truth
impl From<&Config> for RouterConfig {
    fn from(config: &Config) -> Self {
        Self {
            prefix_delimiter: config.prefix_delimiter.clone(),
            priority_tools: config.priority_tools.clone(),
            tool_description_max_chars: config.tool_description_max_chars,
            tool_search_threshold: config.tool_search_threshold,
            tool_filter_enabled: config.tool_filter_enabled,
        }
    }
}

// Both callsites now use:
let router_config = RouterConfig::from(&config);
```

### Why This Works

The `From` trait is idiomatic Rust for struct-to-struct conversion. Placing the impl next to `RouterConfig` in `proxy/mod.rs` centralizes the mapping. The dependency direction (`proxy` → `config`) matches the existing module graph — no circular dependencies introduced.

---

## Fix #020: Tracing Init Split for Daemon vs CLI

### Root Cause

`main()` unconditionally initialized stderr tracing before command dispatch. Since `tracing_subscriber` only allows one global subscriber, `daemon::setup_file_logging()` was dead code — annotated `#[allow(dead_code)]` because calling it after stderr init would panic or silently fail.

### Solution

Defer tracing initialization based on command:

```rust
// main.rs — command-aware tracing setup
let daemon_mode = matches!(&cli.command, Commands::Serve { daemon: true, .. });
let _daemon_log_guard = if daemon_mode {
    Some(daemon::setup_file_logging(&daemon::log_dir())?)
} else {
    init_stderr_tracing(cli.verbose);
    None
};
```

Daemon gets JSON file logging with daily rotation:

```rust
// daemon.rs — file-based logging for headless mode
pub fn setup_file_logging(log_directory: &Path) -> anyhow::Result<WorkerGuard> {
    ensure_dir(log_directory)?;
    let file_appender = tracing_appender::rolling::daily(log_directory, "plug.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    // ... subscriber with .json() format ...
    Ok(guard)  // Must be held for process lifetime
}
```

### Why This Works

The `_daemon_log_guard` binding keeps the `WorkerGuard` alive for the entire `main()` scope, ensuring log flushing. The `matches!` check runs before any async work. If `setup_file_logging` fails (permission denied), the error propagates before any subscriber is installed — clean fail-fast.

---

## Prevention Rules

1. **Return bool from state-mutation methods** — callers decide whether to set dirty, never set it unconditionally.
2. **DRY type conversions via `From` trait** — if a struct is built from another struct in 2+ places, add `impl From<&T>` at the target type's definition.
3. **Defer global singleton init until command dispatch** — never initialize tracing/logging before knowing which mode will run.
4. **Hold guards for process lifetime** — `tracing_appender::WorkerGuard` must survive the entire command, not just setup.
5. **Events without visual effect skip dirty flag** — explicitly handle them as no-ops rather than catching them in a wildcard arm.

## Related Documentation

- [Phase 4 TUI Dashboard & Daemon Patterns](../integration-issues/phase4-tui-dashboard-daemon-patterns.md) — dirty-flag event batching, daemon architecture
- [rmcp SDK Integration Patterns](../integration-issues/rmcp-sdk-integration-patterns-plug-20260303.md) — ArcSwap atomic snapshots, config loading
- [Phase 3 Resilience & Token Efficiency](../integration-issues/phase3-resilience-token-efficiency.md) — RouterSnapshot pre-caching, DashMap vs ArcSwap split
- [docs/ARCHITECTURE.md](../../ARCHITECTURE.md) — event bus design, concurrency model
- [docs/PLAN.md](../../PLAN.md) — Phase 4 implementation checkboxes
