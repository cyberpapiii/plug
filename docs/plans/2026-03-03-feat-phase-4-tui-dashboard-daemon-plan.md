---
title: "feat: Phase 4 TUI Dashboard and Daemon Mode"
type: feat
status: active
date: 2026-03-03
---

# feat: Phase 4 TUI Dashboard and Daemon Mode

## Enhancement Summary

**Deepened on:** 2026-03-03
**Sections enhanced:** All 5 sub-phases + system-wide impact + risk analysis
**Research agents used:** 11 (ratatui docs, daemon/IPC, event bus patterns, architecture strategist, security sentinel, code simplicity, pattern recognition, performance oracle, agent-native parity, Phase 2/3 learnings)

### Key Improvements

1. **Engine encapsulation** — Make all Engine fields private, expose query methods + `EngineSnapshot`. Prevents TUI from reaching into DashMap internals.
2. **TaskTracker for ordered shutdown** — Add `tokio_util::task::TaskTracker` to Engine for guaranteed task drain before server shutdown.
3. **Security hardening** — Socket path moved from `/tmp` to `~/.local/state/plug/` (symlink attack prevention), IPC message size limits, `flock` via `fs2` crate for PID file exclusion, IPC auth token for privileged commands.
4. **Event correlation** — Add `call_id: u64` to `ToolCallStarted`/`ToolCallCompleted` for matching concurrent tool calls in activity log.
5. **TUI event loop fix** — Replace blocking `events.next().await` with `tokio::select!` over 3 sources (crossterm, engine broadcast, tick interval) + dirty-flag rendering.
6. **Broadcast Lagged recovery** — Add `app.full_refresh(&engine)` path when `RecvError::Lagged` is received.
7. **ratatui 0.30 API migration** — `Block::title` takes `Line`, `highlight_symbol` takes `Into<Line>`, use `ratatui::init()` for panic hooks.
8. **Drop Doctor view** — Defer to Phase 5 per `docs/PLAN.md`. Reduces scope.
9. **Arc\<str\> for event strings** — O(1) clone instead of O(n) String clone on broadcast fan-out.

### New Risks Discovered

- **CRITICAL**: `/tmp/plug-$UID.sock` is vulnerable to symlink attack — use `~/.local/state/plug/` instead
- **CRITICAL**: IPC framing must use length-prefixed JSON (not NDJSON) — embedded newlines in JSON strings break line-delimited framing; 4MB max frame enforced at read
- **HIGH**: IPC `Shutdown` command has no authentication — child MCP servers could kill daemon
- **HIGH**: Daemon log files may leak tool call arguments containing secrets
- **Pre-existing bug**: DashMap guard held across `.await` in `health.rs:94` — fix during Engine extraction

### Technical Review Findings Applied (2026-03-04)

Findings from 6-agent technical review (architecture, security, performance, simplicity, agent-native, learnings). Key changes:

1. **IPC framing**: NDJSON → length-prefixed JSON (4-byte big-endian u32 prefix + payload, 4MB max)
2. **Auth token**: 256-bit entropy from `OsRng`, `auth_token` field added to privileged IpcRequest variants, constant-time comparison
3. **`ToggleServer` → `SetServerEnabled`**: Data-in (explicit `enabled: bool`) not decision-in; classified as privileged
4. **Event batching**: `try_recv()` drain loop after each `engine_rx.recv()` wake
5. **SessionManager**: Kept outside Engine (transport-specific); Engine holds only transport-agnostic state
6. **IPC timeouts**: 60s idle, 30s partial message timeout per connection
7. **Restart rate limit**: Moved from TUI `App` to `Engine::restart_server()` (applies to IPC too)
8. **Health debouncing**: Transition-based (emit on `old != new`) instead of 5s time suppression
9. **Subscribe**: Implemented in Phase 4 (not deferred) for agent-native parity

---

## Overview

Phase 4 adds a ratatui-powered TUI dashboard and headless daemon mode to plug. This builds on Phase 1 (stdio proxy, PR #1), Phase 2 (HTTP transport, PR #2), and Phase 3 (resilience + token efficiency, PR #3).

The central architectural change is extracting shared logic from `cmd_connect` and `cmd_serve` into a unified **Engine** struct with an **event bus** (`tokio::sync::broadcast`). The TUI, daemon, and CLI become thin frontends over the same Engine. This is the lazygit/gitui pattern — clean model/view separation.

New crates: `ratatui 0.30`, `crossterm 0.29` (async EventStream), `tracing-appender` (file logging), `tokio-util` (codec for IPC), `fs2` (file locking).

## Problem Statement

Currently plug has no unified runtime:
- `cmd_connect` (stdio) and `cmd_serve` (HTTP) inline all startup logic — server init, tool refresh, health checker spawning
- No way to observe system state in real-time (health transitions, circuit breaker events, tool cache refreshes)
- No daemon mode — each `plug connect` is a standalone process
- `plug status` cannot query a running instance
- No visual dashboard for monitoring servers, clients, and activity

## Proposed Solution

Four implementation sub-phases (collapsed from original 5 per simplicity review — merge C+D):

**Sub-phase A (Engine Extraction)**: Extract Engine struct + event bus from cmd_connect/cmd_serve
**Sub-phase B (TUI Framework + Dashboard)**: ratatui + crossterm async event loop, App state, responsive layout, all panel widgets
**Sub-phase C (Interactivity + Polish)**: Server management, search, keybindings, colors, help overlay
**Sub-phase D (Daemon Mode + IPC)**: Headless mode, Unix socket IPC, PID lifecycle, file logging

## Technical Approach

### Architecture

```
┌──────────────────────────────────────────────┐
│                   plug binary                 │
│                                              │
│  ┌─────────┐  ┌─────────┐  ┌─────────────┐  │
│  │   TUI   │  │  Daemon  │  │    CLI      │  │
│  │(ratatui)│  │(headless)│  │ (plug ...)  │  │
│  └────┬────┘  └────┬────┘  └──────┬──────┘  │
│       │            │               │         │
│       ▼            ▼               │         │
│  ┌─────────────────────────┐       │         │
│  │    Engine (plug-core)   │◄──────┘         │
│  │                         │   (via IPC)     │
│  │  ServerManager          │                 │
│  │  ToolRouter             │                 │
│  │  SessionManager         │                 │
│  │  TaskTracker             │                 │
│  │  EventBus (broadcast)   │                 │
│  └─────────────────────────┘                 │
└──────────────────────────────────────────────┘
```

### Implementation Phases

#### Sub-phase A: Engine Extraction

**Goal**: Unified Engine struct that owns all shared state, replacing inlined logic in cmd_connect/cmd_serve.

**Tasks:**

1. **Create `plug-core/src/engine.rs`** — Engine struct with **private fields**:
   ```rust
   pub struct Engine {
       server_manager: Arc<ServerManager>,
       tool_router: Arc<ToolRouter>,
       // NOTE: SessionManager is NOT in Engine — it's transport-specific.
       // HTTP creates its own SessionManager; stdio has implicit single session.
       // Engine only tracks session count via events.
       config: Arc<ArcSwap<Config>>,     // ArcSwap for hot-reload support
       cancel: CancellationToken,
       tracker: TaskTracker,              // from tokio-util — ordered shutdown
       event_tx: broadcast::Sender<EngineEvent>,
       started_at: Instant,
   }
   ```

2. **Define `EngineEvent` enum** in `plug-core/src/engine.rs`:
   ```rust
   #[derive(Clone, Debug, Serialize, Deserialize)]
   pub enum EngineEvent {
       ServerHealthChanged { server_id: Arc<str>, old: ServerHealth, new: ServerHealth },
       CircuitBreakerTripped { server_id: Arc<str>, state: CircuitState },
       ToolCacheRefreshed { tool_count: usize },
       ClientConnected { session_id: String, client_type: ClientType },
       ClientDisconnected { session_id: String },
       ToolCallStarted { call_id: u64, server_id: Arc<str>, tool_name: Arc<str> },
       ToolCallCompleted { call_id: u64, server_id: Arc<str>, tool_name: Arc<str>, duration_ms: u64, success: bool },
       ServerStarted { server_id: Arc<str> },
       ServerStopped { server_id: Arc<str> },
       Error { context: Arc<str>, message: Arc<str> },
       ConfigReloaded,
   }
   ```

3. **Engine query API** — all state access through methods, never direct field access:
   ```rust
   impl Engine {
       pub fn snapshot(&self) -> EngineSnapshot { ... }
       pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> { ... }
       pub fn server_statuses(&self) -> Vec<ServerStatus> { ... }
       pub fn tool_list(&self) -> Arc<Vec<Tool>> { ... }
       pub fn session_count(&self) -> usize { ... }
       pub fn cancel_token(&self) -> &CancellationToken { &self.cancel }
       pub async fn restart_server(&self, id: &str) -> Result<()> { ... }  // rate-limited: 1 per server per 10s
       pub async fn set_server_enabled(&self, id: &str, enabled: bool) -> Result<()> { ... }
   }
   ```

4. **EngineSnapshot** — read-only, Clone-able view for TUI initial state:
   ```rust
   #[derive(Clone, Debug)]
   pub struct EngineSnapshot {
       pub servers: Vec<ServerStatus>,
       pub session_count: usize,
       pub tool_count: usize,
       pub uptime: Duration,
   }
   ```

5. **Engine::start()** — async method that starts all servers (replaces logic in cmd_connect and cmd_serve). Uses `self.tracker.spawn()` for all background tasks.

6. **Engine::shutdown()** — ordered shutdown with TaskTracker:
   ```rust
   pub async fn shutdown(&self) {
       self.cancel.cancel();
       self.tracker.close();
       let _ = tokio::time::timeout(Duration::from_secs(5), self.tracker.wait()).await;
       self.server_manager.shutdown_all().await;
   }
   ```

7. **Instrument existing code** — Add `let _ = event_tx.send()` calls in:
   - `health.rs` — on health transitions (emit from health checker, NOT inside HealthState)
   - `proxy/mod.rs` — on tool calls (emit from `ToolRouter::call_tool()`, NOT inside CircuitBreaker)
   - `server/mod.rs` — on server start/stop
   - `http/session.rs` — on client connect/disconnect
   - `proxy/mod.rs` — pass `event_tx` to ProxyHandler at construction for stdio ClientConnected events

8. **Fix pre-existing bug** — `health.rs:94` holds DashMap guard across `router.refresh_tools().await`. Extract health state to local vars, drop guard before `.await`.

9. **Refactor `cmd_connect` and `cmd_serve`** in the `plug` binary to use `Engine::new()` + `Engine::start()`.

**Files changed:**
- NEW: `plug-core/src/engine.rs`
- EDIT: `plug-core/src/lib.rs` (add `pub mod engine`)
- EDIT: `plug-core/src/health.rs` (accept event_tx, tracker; fix DashMap guard bug)
- EDIT: `plug-core/src/proxy/mod.rs` (accept event_tx, send events; add call_id counter)
- EDIT: `plug-core/src/server/mod.rs` (accept event_tx, send events)
- EDIT: `plug-core/src/http/server.rs` (HttpState owns SessionManager, not Engine)
- EDIT: `plug/src/main.rs` (refactor cmd_connect/cmd_serve to use Engine)

**Tests:**
- Engine creation with mock config
- Event bus subscription and receive (use `collect_events` helper with timeout)
- Engine::shutdown() waits for tasks via TaskTracker
- broadcast::RecvError::Lagged recovery
- Backward compatibility: cmd_connect and cmd_serve still work identically

**Success criteria:** `cargo test` passes, `plug connect` and `plug serve` work exactly as before, events flow on broadcast channel.

#### Research Insights: Engine Extraction

**Architecture Review Findings:**
- Engine fields MUST be private. Public fields allow TUI to bypass event bus and read DashMaps directly, creating tight coupling to ServerManager internals. Expose query methods returning value types instead.
- `TaskTracker` is required — `CancellationToken` alone signals tasks to stop but does not wait for completion. Health checkers could be mid-flight when `shutdown_all()` is called.
- Use `ArcSwap<Config>` instead of `Arc<Config>` per ARCHITECTURE.md. Hot-reload is a documented design goal; this prevents a migration later.
- `ToolCallStarted`/`ToolCallCompleted` need a `call_id: u64` (monotonic counter) for correlation. Without it, concurrent calls to the same tool on the same server are indistinguishable in the activity log.
- Add `Error` and `ConfigReloaded` event variants — the TUI needs a way to display operational errors without subscribing to tracing logs separately.

**Phase 2/3 Learnings Applied:**
- Emit circuit breaker events from `ToolRouter::call_tool()` call site, NOT inside `CircuitBreaker`. The circuit breaker is a lock-free atomic state machine — it should not own the event_tx.
- Use `.clone().cancelled_owned()` for `'static` futures in spawned tasks.
- `biased` select for shutdown priority in all event loops.
- Clone-and-drop pattern for DashMap guards: extract data into local variables, drop guard, then use data across `.await`.
- `Arc::try_unwrap` + `TaskTracker` for ordered Engine shutdown.

**Performance Review:**
- Use `Arc<str>` for string fields in EngineEvent. With 3+ broadcast receivers, each `String` clone allocates. `Arc<str>` costs only an atomic increment. Create via `Arc::from("server_id")`.
- Event emission pattern must be `let _ = self.event_tx.send(...)` — silently drop errors when no receivers exist (startup/shutdown).

**Event Bus Research:**
- `broadcast` capacity 128 is optimal. At peak burst (~130 events/sec with 20 servers), this provides ~1 second of buffer. Memory cost: ~25KB. No need for 256 or 1024.
- Broadcast is correct for observability events — producers must never block, consumers can tolerate gaps.
- For `Lagged` recovery: log the gap count, then call `app.full_refresh(&engine)` to reconcile state from Engine::snapshot().

---

#### Sub-phase B: TUI Framework + Dashboard

**Goal**: ratatui + crossterm async event loop with responsive dashboard layout and all panel widgets.

**Tasks:**

1. **Add dependencies to `plug/Cargo.toml`:**
   - `ratatui = "0.30"` (includes crossterm backend)
   - `crossterm = { version = "0.29", features = ["event-stream"] }`
   - `color-eyre = "0.6"` (for panic hooks + error reporting)

2. **Create `plug/src/tui/mod.rs`** — TUI module structure:
   ```
   plug/src/tui/
   ├── mod.rs          // pub mod, terminal init/restore
   ├── app.rs          // App state, handle_input(), handle_engine_event(), view()
   ├── event.rs        // AppEvent enum (not needed if using tokio::select! directly)
   ├── widgets/
   │   ├── mod.rs
   │   ├── servers.rs  // Servers panel widget
   │   ├── clients.rs  // Clients panel widget
   │   ├── activity.rs // Activity log widget
   │   ├── tools.rs    // Tools list + detail
   │   ├── logs.rs     // Structured log viewer
   │   └── help.rs     // Help overlay
   └── theme.rs        // Theme struct, colors, symbols, NO_COLOR
   ```

3. **Main TUI loop** — `tokio::select!` with 3 sources:
   ```rust
   async fn cmd_tui(config: Config) -> Result<()> {
       color_eyre::install()?;
       let engine = Arc::new(Engine::new(config)?);
       engine.start().await?;

       let mut terminal = ratatui::init();
       let mut app = App::new(engine.snapshot());  // populate initial state

       let mut engine_rx = engine.subscribe();
       let mut crossterm_events = crossterm::event::EventStream::new();
       let mut tick = tokio::time::interval(Duration::from_millis(250));
       tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

       loop {
           if app.dirty {
               terminal.draw(|f| app.view(f))?;
               app.dirty = false;
           }

           tokio::select! {
               biased;

               _ = engine.cancel_token().cancelled() => break,

               Some(Ok(event)) = crossterm_events.next() => {
                   app.handle_input(event);
               }

               result = engine_rx.recv() => {
                   match result {
                       Ok(event) => {
                           app.handle_engine_event(event);
                           // Drain any queued events before rendering (event batching)
                           while let Ok(event) = engine_rx.try_recv() {
                               app.handle_engine_event(event);
                           }
                       }
                       Err(broadcast::error::RecvError::Lagged(n)) => {
                           tracing::warn!(skipped = n, "TUI lagged, refreshing");
                           app.full_refresh(&engine);
                       }
                       Err(broadcast::error::RecvError::Closed) => break,
                   }
               }

               _ = tick.tick() => {
                   app.tick();
               }
           }

           if app.should_quit { break; }
       }

       ratatui::restore();
       engine.shutdown().await;
       Ok(())
   }
   ```

4. **App state** (`plug/src/tui/app.rs`):
   ```rust
   pub struct App {
       pub mode: AppMode,
       pub dirty: bool,
       pub focused_panel: usize,
       pub server_list: StatefulList<ServerInfo>,
       pub client_list: StatefulList<ClientInfo>,
       pub activity_log: VecDeque<ActivityEntry>,  // Rolling, max 1000
       pub tool_list: StatefulList<ToolInfo>,
       pub search_query: Option<String>,
       pub should_quit: bool,
   }

   pub enum AppMode {
       Dashboard,
       Tools,
       ToolDetail(String),
       Logs,
       Help,          // overlay mode
   }
   ```
   - `App::new(snapshot)` — populate initial state from `EngineSnapshot`
   - `App::full_refresh(engine)` — re-populate from Engine query methods on Lagged recovery
   - `App::handle_input()` — key/mouse events → mutate state, set `dirty = true`
   - `App::handle_engine_event()` — update local copies of server/client/activity state
   - `App::tick()` — expire flash highlights, return bool indicating change
   - `App::view()` — render from local state only, never reach back to Engine

5. **Dashboard layout** — responsive based on terminal width:
   - **Wide (>= 120 cols)**: 3-column — servers | clients | activity
   - **Medium (80-119 cols)**: 2-row — [servers + clients] above, activity below
   - **Narrow (< 80 cols)**: Tabbed — one panel at a time, Tab to switch

   ```rust
   fn compute_layout(area: Rect) -> Vec<Rect> {
       if area.width >= 120 {
           Layout::horizontal([
               Constraint::Percentage(33),
               Constraint::Percentage(34),
               Constraint::Percentage(33),
           ]).split(area).to_vec()
       } else if area.width >= 80 {
           let rows = Layout::vertical([
               Constraint::Percentage(60),
               Constraint::Percentage(40),
           ]).split(area);
           let cols = Layout::horizontal([
               Constraint::Percentage(50),
               Constraint::Percentage(50),
           ]).split(rows[0]);
           vec![cols[0], cols[1], rows[1]]
       } else {
           // Tabbed: render only focused panel in full area
           vec![area]
       }
   }
   ```

6. **Servers panel** (`plug/src/tui/widgets/servers.rs`):
   ```
   ┌ Servers ─────────────────────┐
   │ ● github        12 tools  3ms│
   │ ● filesystem     8 tools  1ms│
   │ ◐ postgres       5 tools 45ms│
   │ ○ notion         0 tools  -- │
   └──────────────────────────────┘
   ```
   - Health indicator: `●` green (Healthy), `◐` yellow (Degraded), `○` red (Failed)
   - Tool count from ToolRouter cache
   - Latency from last health check round-trip

7. **Clients panel** (`plug/src/tui/widgets/clients.rs`):
   ```
   ┌ Clients ─────────────────────┐
   │ Claude Code  abc-123  52 tools│
   │ Cursor       def-456  40 tools│
   │ Gemini CLI   ghi-789  25 tools│
   └──────────────────────────────┘
   ```

8. **Activity panel** (`plug/src/tui/widgets/activity.rs`):
   ```
   ┌ Activity ────────────────────┐
   │ 12:34:56 github → create_issue  OK 45ms│
   │ 12:34:55 fs → read_file         OK  2ms│
   │ 12:34:50 postgres → query       ERR 30s│
   └──────────────────────────────┘
   ```
   - Rolling log of tool calls (VecDeque, max 1000 entries)
   - Color-coded: green OK, red ERR

9. **Tools view** — full-screen mode with search and detail drill-down.

10. **Log view** — full-screen structured log with level/server/client filters.

11. **Status bar** — bottom row, context-aware keybindings.

12. **Navigation** — vim-inspired keybindings:
   - `q` / `Ctrl-C` — quit
   - `Tab` / `Shift-Tab` — cycle panels
   - `j` / `k` / `Up` / `Down` — navigate within panel
   - `Enter` — select/drill into
   - `Esc` — back to dashboard
   - `1-4` — jump to panel
   - `/` — search mode
   - `?` — help overlay
   - `r` — restart selected server
   - `d` — disable/enable selected server

**Files created:**
- `plug/src/tui/mod.rs`
- `plug/src/tui/app.rs`
- `plug/src/tui/theme.rs`
- `plug/src/tui/widgets/mod.rs`
- `plug/src/tui/widgets/servers.rs`
- `plug/src/tui/widgets/clients.rs`
- `plug/src/tui/widgets/activity.rs`
- `plug/src/tui/widgets/tools.rs`
- `plug/src/tui/widgets/logs.rs`
- `plug/src/tui/widgets/help.rs`

**Tests:**
- App state transitions (mode changes, navigation)
- Layout selection at 80, 120, 200 column widths
- Server panel rendering with mock data (all health states)
- Activity log rolling (>1000 entries)
- App::full_refresh() from EngineSnapshot
- Theme respects NO_COLOR

**Success criteria:** `plug tui` launches, shows dashboard with servers/clients/activity, responds to key presses, quits cleanly with `q`. Renders correctly at 80x24, 120x40, 200x60.

#### Research Insights: TUI Framework

**ratatui 0.30 API Changes (Breaking):**
- `Block::title()` now takes `Line` directly — `Title` struct removed. Use `Block::bordered().title(Line::from(" Servers "))`
- `List::highlight_symbol` now accepts `Into<Line>` — use `Line::from(">> ")` for forward compat
- `Alignment` renamed to `HorizontalAlignment` — type alias exists but update for exhaustive matches
- `Flex::SpaceAround` changed behavior — use `Flex::SpaceEvenly` for old behavior
- MSRV 1.86.0, Rust 2024 edition — compatible with project settings

**Terminal Setup Best Practice:**
- Use `ratatui::init()` which auto-installs panic hook for terminal restore. Call `color_eyre::install()` FIRST (before `ratatui::init()`) so terminal is restored before error report prints.
- Do NOT manually call `enable_raw_mode()`, `EnterAlternateScreen` — `ratatui::init()` handles it.
- Use `ratatui::restore()` on exit (not a custom Drop impl).

**Dirty Flag Rendering (Performance):**
- Only call `terminal.draw()` when `app.dirty == true`. ratatui's double-buffer diffing is fast (~5ms) but the terminal write syscall is the real bottleneck.
- Set `dirty = true` on: any key press, any engine event, resize events.
- Set `dirty = false` after each draw.
- The `tick()` handler should return whether state changed (flash expiry), and only set dirty if true.

**Component Pattern:**
- Each widget file (servers.rs, clients.rs, etc.) should own its `ListState`/`TableState`. The focused panel receives key events; unfocused panels retain scroll position.
- Use `frame.render_stateful_widget(list, area, &mut self.state)` for scrollable lists.

**Event Loop Pattern:**
- Separate tick (250ms, for state polling/animation) from render (driven by events, not fixed FPS). Render after every event that sets `dirty = true`.
- Use `biased` select — cancel signal checked first.
- The TUI should NEVER read DashMap directly. All state comes from Engine events + Engine::snapshot() for initial population.
- Event batching: if burst of engine events arrives between renders, process all before rendering. The `tokio::select!` naturally handles this — multiple `recv()` calls between `draw()` calls.

---

#### Sub-phase C: Interactivity and Visual Polish

**Goal**: Server management from TUI, search, help overlay, visual indicators.

**Tasks:**

1. **Server management**:
   - `r` on selected server → restart (calls `Engine::restart_server(id)`)
   - `d` on selected server → disable/enable (calls `Engine::set_server_enabled(id, !current)`)
   - Confirmation prompt: require `y` + Enter (not just keypress), display server name
   - Rate limit: enforced in `Engine::restart_server()` (not TUI), at most 1 restart per server per 10 seconds — applies to both TUI and IPC callers

2. **Search** (`/`):
   - Activates search bar in current panel
   - Real-time filtering as you type
   - `Enter` to confirm, `Esc` to cancel
   - Works in: tools view, log view, activity panel

3. **Help overlay** (`?`):
   - Rendered as overlay on current view
   - All keybindings grouped by context (global, dashboard, tools, logs)
   - `?` or `Esc` to dismiss

4. **Theme struct** (`plug/src/tui/theme.rs`):
   ```rust
   pub struct Theme {
       pub healthy: Style,    // green
       pub degraded: Style,   // yellow
       pub failed: Style,     // red
       pub info: Style,       // cyan
       pub dim: Style,        // dark gray
       pub highlight: Style,  // reversed for selection
       pub border: Style,     // gray
       pub normal: Style,     // default
   }

   impl Theme {
       pub fn detect() -> Self {
           if std::env::var("NO_COLOR").map(|v| !v.is_empty()).unwrap_or(false) {
               Self::no_color()
           } else {
               Self::colored()
           }
       }
   }
   ```
   - Health indicators: `●` (green), `◐` (yellow), `○` (red), `↔` (cyan, half-open)
   - `NO_COLOR` → `Style::default()` everywhere + `Modifier::REVERSED` for selection

5. **Health event emission** — transition-based, not time-based (emit only when `old != new`):
   ```rust
   // Emit on actual state transitions only — no time suppression
   if old_health != new_health {
       let _ = event_tx.send(EngineEvent::ServerHealthChanged {
           server_id: Arc::from(server_id.as_str()),
           old: old_health,
           new: new_health,
       });
   }
   ```
   Time-based suppression (5s) was too aggressive — dropped important transitions. Transition-based emission naturally deduplicates since consecutive identical states don't fire.

6. **Flash highlights** — health changes and new activity entries highlighted for 2 ticks (500ms), expired in `App::tick()`.

**Files edited:**
- `plug/src/tui/theme.rs` (Theme struct, colors, symbols, NO_COLOR)
- `plug/src/tui/app.rs` (search state, server management actions, confirmation prompts)
- `plug/src/tui/widgets/help.rs` (help overlay content)
- `plug-core/src/engine.rs` (restart_server, toggle_server methods)
- `plug-core/src/health.rs` (event debouncing)

**Tests:**
- NO_COLOR disables all styling
- Search filters correctly
- Server restart triggers Engine events
- Restart rate-limiting
- Flash highlight expiry

**Success criteria:** All keybindings work. Help overlay shows correct bindings. Colors render correctly and respect NO_COLOR.

---

#### Sub-phase D: Daemon Mode and IPC

**Goal**: Headless daemon with Unix socket IPC for CLI queries.

**Tasks:**

1. **Daemon mode** — `plug serve --daemon` or `plug daemon start`:
   - Same Engine, no TUI (no terminal setup)
   - Structured logging to file via `tracing-appender`
   - Background process (user manages via systemd/launchd)
   - SIGTERM handler for graceful shutdown (systemd sends SIGTERM, not SIGINT):
     ```rust
     #[cfg(unix)]
     async fn shutdown_signal(cancel: CancellationToken) {
         use tokio::signal::unix::{signal, SignalKind};
         let mut sigterm = signal(SignalKind::terminate()).unwrap();
         let mut sigint = signal(SignalKind::interrupt()).unwrap();
         tokio::select! {
             _ = sigterm.recv() => {},
             _ = sigint.recv() => {},
             _ = cancel.cancelled() => {},
         }
         cancel.cancel();
     }
     ```

2. **File logging**:
   ```rust
   let file_appender = rolling::daily(log_dir, "plug.log");
   let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
   // CRITICAL: hold _guard for entire daemon lifetime
   ```
   - Log directory: `$XDG_STATE_HOME/plug/logs/` (macOS: `~/Library/Logs/plug/`)
   - Daily rotation, JSON format
   - Log file permissions: `0600`
   - Log directory permissions: `0700`
   - **Never log tool call arguments at INFO level or below** — arguments may contain secrets

3. **Unix socket IPC** — CLI → daemon communication:
   - Socket path: `$XDG_RUNTIME_DIR/plug/plug.sock` (macOS: `~/Library/Application Support/plug/plug.sock`). **NEVER use `/tmp`** — vulnerable to symlink attacks.
   - Create parent directory with `0700` permissions
   - Set socket permissions to `0600` after bind
   - Protocol: **length-prefixed JSON** (4-byte big-endian u32 length prefix + JSON payload). NDJSON rejected because embedded newlines in JSON strings break line-delimited framing (per `docs/research/daemon-architecture.md` recommendation).
   - Read: `read_u32()` → reject if > 4MB → `read_exact(len)` → `serde_json::from_slice()`
   - Write: serialize → `write_u32(len)` → `write_all(payload)`
   - Request/response pattern:

   ```rust
   // plug-core/src/ipc.rs — shared types
   #[derive(Serialize, Deserialize)]
   #[serde(tag = "type")]
   pub enum IpcRequest {
       // Read-only (no auth required)
       Status,
       ServerList,
       ClientList,
       ToolList,
       Subscribe,      // stream EngineEvents over socket
       // Mutating (auth_token required)
       RestartServer { server_id: String, auth_token: String },
       SetServerEnabled { server_id: String, enabled: bool, auth_token: String },
       Shutdown { auth_token: String },
   }

   #[derive(Serialize, Deserialize)]
   #[serde(tag = "type")]
   pub enum IpcResponse {
       Status {
           servers: Vec<ServerStatus>,
           clients: usize,
           uptime_secs: u64,
       },
       ServerList { servers: Vec<ServerStatus> },
       ClientList { clients: Vec<ClientInfo> },
       ToolList { tools: Vec<ToolInfo> },
       Ok,
       Error { code: String, message: String },  // machine-parseable error code
       Event(EngineEvent),  // streamed when Subscribe is active
   }
   ```

4. **IPC security**:
   - **Max frame size**: 4MB enforced at length-prefix read (reject before allocating buffer)
   - **Server ID validation**: reject `RestartServer`/`SetServerEnabled` if server_id not in running server set
   - **Connection limit**: Semaphore with 32 permits for concurrent IPC connections
   - **Connection timeouts**: 60s idle timeout (no complete message received), 30s partial message timeout (bytes received but no complete frame)
   - **Privileged command auth**: Generate 256-bit (32-byte) random token at daemon startup via `rand::rngs::OsRng`, hex-encode (64 chars), write to `~/.local/state/plug/plug.token` with `0600` perms.
     - **Privileged**: `Shutdown`, `RestartServer`, `SetServerEnabled` — require `auth_token` field
     - **Read-only**: `Status`, `ServerList`, `ClientList`, `ToolList`, `Subscribe` — no auth
     - **Constant-time comparison**: Use `subtle::ConstantTimeEq` or equivalent to prevent timing side-channel
     - Add `rand` and `subtle` to crate dependencies

5. **Daemon lifecycle**:
   - On start: create parent dirs (`0700`), bind Unix socket, use `fs2::FileExt::try_lock_exclusive()` on PID file, write PID
   - Write PID file AFTER successful socket bind (not before)
   - On shutdown: remove socket + PID file, release lock
   - Detection: `plug status` tries socket connect first (liveness), falls back to PID file
   - Socket liveness > PID file — PID files can be stale after crashes
   - Stale socket: verify parent dir ownership + permissions before cleanup

6. **CLI commands** that talk to daemon:
   - `plug status` — query running daemon via IPC, show server/client status
   - `plug server restart <name>` — tell daemon to restart a server
   - `plug daemon stop` — graceful shutdown via IPC
   - If no daemon running, these commands print "No daemon running" and exit

7. **Crate boundary**: IPC types (`IpcRequest`, `IpcResponse`) go in `plug-core/src/ipc.rs`. Socket listener and daemon lifecycle go in `plug/src/daemon.rs`. This keeps the `libc`/`fs2` dependency out of plug-core.

**Files created:**
- `plug-core/src/ipc.rs` (IpcRequest, IpcResponse, validation)
- `plug/src/daemon.rs` (daemon startup, IPC listener, PID file, socket management)

**Files edited:**
- `plug/Cargo.toml` (add `tracing-appender`, `fs2`, `color-eyre`, `rand`, `subtle`, `hex`)
- `plug/src/main.rs` (daemon/tui mode selection, status command, SIGTERM handler)

**Tests:**
- IPC request/response serialization round-trip (length-prefixed framing)
- Socket creation and cleanup (including permission verification)
- Daemon start → status query → shutdown lifecycle
- PID file locking (concurrent start prevention)
- Server ID validation in RestartServer/SetServerEnabled
- Max frame size enforcement (reject > 4MB at length-prefix read)
- Auth token validation for privileged commands (constant-time comparison)
- Connection limit (Semaphore permits)
- Connection idle timeout (60s)
- Subscribe → receive EngineEvent stream

**Success criteria:** `plug serve --daemon` starts headless, `plug status` returns server info via IPC, `plug daemon stop` shuts down cleanly.

#### Research Insights: Daemon Mode + IPC

**Security Review Findings (Ranked by Risk):**

| # | Finding | Severity | Resolution |
|---|---------|----------|------------|
| 1 | Socket path `/tmp/plug-$UID.sock` vulnerable to symlink attack | **Critical** | Use `~/.local/state/plug/` with `0700` parent dir |
| 2 | IPC framing must handle embedded newlines safely | **Critical** | Length-prefixed JSON (4-byte u32 prefix + payload, 4MB max frame) |
| 3 | PID file TOCTOU race, no `flock` | **High** | Use `fs2::FileExt::try_lock_exclusive()` |
| 4 | Log files may leak tool call arguments | **High** | Never log arguments at INFO or below; redact patterns matching `*_TOKEN`, `*_SECRET`, `*_KEY` |
| 5 | IPC `Shutdown` has no authentication | **High** | Auth token for privileged commands |
| 6 | `RestartServer` accepts arbitrary server_id | **High** | Validate against running server set |
| 7 | No IPC connection rate limit | **Low** | Semaphore with 32 permits |

**Architecture Review Findings:**
- Socket path resolution needs platform-aware fallback without `unsafe` (use `nix` crate or keep in binary crate). Since `#![forbid(unsafe_code)]` is project-wide, use `fs2` (pure-safe Rust) for file locking.
- PID file should be written AFTER socket bind succeeds — socket bind is the actual exclusion mechanism.
- Implement `IpcRequest::Subscribe` and `IpcResponse::Event` in Phase 4 — required for agent-native parity (`plug status --follow`).
- Length-prefixed JSON (4-byte u32 + payload) is safer than NDJSON — immune to embedded newlines. Test with a small Rust client or `plug status` CLI command.

**Agent-Native Parity:**
- 7 of 12 TUI capabilities had NO IPC path in original plan. Added: `ClientList`, `SetServerEnabled`, `Subscribe`. The IPC protocol should mirror every TUI action so agents can manage plug programmatically.
- `EngineEvent` needs `Serialize`/`Deserialize` for IPC streaming via `IpcResponse::Event`.

---

## System-Wide Impact

### Interaction Graph

- Engine::start() → `tracker.spawn()` health checker → health transitions send EngineEvent → TUI receives via broadcast → sets `dirty = true` → re-renders on next loop
- ToolRouter::call_tool() → circuit breaker check → semaphore acquire → upstream call → on_success/on_failure → EngineEvent::ToolCallCompleted (with call_id) → TUI activity log
- Client connects (stdio) → ProxyHandler::initialize() → `event_tx.send(ClientConnected)` → TUI clients panel
- Client connects (HTTP) → SessionManager → EngineEvent::ClientConnected → TUI clients panel
- `plug status` CLI → Unix socket connect → IpcRequest::Status → Engine::snapshot() → IpcResponse::Status → CLI prints

### Error & Failure Propagation

- broadcast `RecvError::Lagged(n)` → TUI logs warning + calls `app.full_refresh(&engine)` to reconcile from Engine::snapshot()
- broadcast `RecvError::Closed` → Engine dropped, TUI exits
- Unix socket connection refused → CLI prints "No daemon running" — no retry
- IPC oversized message (>4MB) → connection closed with Error response
- IPC invalid JSON → connection closed with Error response
- Terminal resize during render → crossterm Resize event → `dirty = true` → re-layout on next draw
- Panic in TUI → `ratatui::init()` panic hook restores terminal before unwinding
- Engine::shutdown() timeout → log warning, proceed with server shutdown (don't block forever on stuck tasks)

### State Lifecycle Risks

- Engine shutdown: cancel tasks → `tracker.close()` → `tracker.wait()` (with 5s timeout) → `shutdown_all()` → remove socket + PID file. TaskTracker ensures health checkers are drained.
- TUI crash: `ratatui::restore()` in normal path, panic hook in crash path. No raw mode leak.
- Stale PID file: socket liveness check resolves. `fs2` flock auto-releases on process exit.
- Daemon env freeze: daemon inherits spawning process environment. Credentials rotated in user's shell are NOT picked up. Document: restart daemon after credential rotation.

### Integration Test Scenarios

1. Start daemon → `plug status` → shows servers → `plug daemon stop` → confirm socket + PID cleanup
2. Start TUI → resize terminal at 80/120/200 → verify layout adapts
3. Kill upstream server → verify TUI shows health degradation → circuit breaker trip → recovery
4. Multiple `plug connect` instances → daemon serves all simultaneously
5. Concurrent daemon start attempt → second instance fails with "already running" (flock)

## Acceptance Criteria

### Functional Requirements

- [ ] Engine struct extracted, cmd_connect and cmd_serve refactored to use it
- [ ] Engine fields are private with query API (snapshot, server_statuses, tool_list, etc.)
- [ ] Event bus delivers health, circuit, tool, client, error events with call_id correlation
- [ ] TUI dashboard shows servers, clients, activity in real-time
- [ ] TUI populates initial state from Engine::snapshot() on startup
- [ ] TUI recovers from broadcast Lagged via full_refresh()
- [ ] Tools view with search and detail drill-down
- [ ] Log view with level/server/client filters
- [ ] Server restart and disable/enable from TUI with confirmation
- [ ] Help overlay with all keybindings
- [ ] Daemon mode with file logging (0600 permissions, no argument leakage)
- [ ] Unix socket IPC with auth token for privileged commands
- [ ] Socket path in user-owned directory (never /tmp)
- [ ] PID file with flock exclusion via fs2

### Non-Functional Requirements

- [ ] TUI renders correctly at 80x24, 120x40, 200x60
- [ ] NO_COLOR support via Theme struct
- [ ] Terminal restored on panic (ratatui::init() panic hook)
- [ ] Broadcast channel handles Lagged receivers with full_refresh
- [ ] Dirty-flag rendering — skip frames when nothing changed
- [ ] File logging with daily rotation
- [ ] IPC max message size 4MB
- [ ] IPC connection limit 32

### Quality Gates

- [ ] All existing 88 tests still pass (no regressions)
- [ ] New tests for Engine (creation, shutdown, events, TaskTracker), App state, IPC round-trip
- [ ] Pre-existing DashMap guard-across-await bug in health.rs fixed
- [ ] `cargo clippy` clean
- [ ] `#![forbid(unsafe_code)]` maintained (use fs2 for flock, not libc)

## Dependencies & Prerequisites

### Crate Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| ratatui | 0.30 | TUI framework (MSRV 1.86.0, Rust 2024 edition) |
| crossterm | 0.29 | Terminal backend, async EventStream |
| color-eyre | 0.6 | Panic hooks + error reporting |
| tracing-appender | 0.2 | File logging with daily rotation |
| fs2 | 0.4 | Safe file locking (flock) for PID file |
| tokio-util | 0.7 | TaskTracker for ordered shutdown |
| rand | 0.8 | Cryptographic random token generation (OsRng) |
| subtle | 2.6 | Constant-time equality for auth token comparison |
| hex | 0.4 | Hex-encode auth token (32 bytes → 64 chars) |

### Prerequisites

- Phase 3 merged (PR #3) — circuit breakers, health checks, tool filtering
- Rust edition 2024 (already configured)
- rmcp 1.0.0 (already in use)

## Risk Analysis & Mitigation

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Socket symlink attack in /tmp | N/A | **Eliminated** | Socket in user-owned dir with 0700 parent |
| IPC message OOM | N/A | **Eliminated** | Length-prefixed JSON, 4MB max enforced at read (reject before allocation) |
| ratatui 0.30 API differs from docs | Low | Medium | Pin version, use `Line::from()` for all titles |
| broadcast Lagged drops TUI state | Medium | Medium | full_refresh() from Engine::snapshot() |
| Terminal corruption on panic | Medium | High | ratatui::init() auto-installs panic hook |
| Engine refactor breaks existing stdio/HTTP | Medium | High | Run all 88 existing tests after refactor |
| PID file race on concurrent start | Low | Medium | fs2 flock exclusion |
| Log files leak secrets | Medium | High | Never log arguments; redact sensitive env patterns |
| IPC shutdown by rogue child process | Low | High | Auth token for privileged commands |
| DashMap guard across .await in health.rs | Confirmed | Medium | Fix during Engine extraction (extract + drop) |

## Key Learnings From Phase 1-3 (Applied)

From `docs/solutions/integration-issues/`:

1. **rmcp API gotchas** — No `CallToolResult::text()`, no `Peer::ping()`. Use `CallToolResult::success(vec![Content::text(...)])` and `list_all_tools()` as ping substitute.
2. **DashMap vs ArcSwap split** — Mutable per-server state in DashMap, immutable snapshots in ArcSwap. Engine follows same pattern. TUI reads snapshots via Engine query methods, never DashMap directly.
3. **Lock-free circuit breaker** — AtomicU8/U32/U64 for state, Semaphore for probes. Drain permits before adding.
4. **SecretString** — Auth tokens use `SecretString` with redacted Debug. IPC auth token should too.
5. **UTF-8 safe iteration** — Advance by `ch.len_utf8()`, never by 1 byte.
6. **RwLock not OnceLock** for mutable state — OnceLock silently discards writes.
7. **DashMap guards across .await** — Never hold guards across async boundaries. Extract data, drop guard, then `.await`.
8. **CancellationToken + TaskTracker** — Cancel signals tasks, TaskTracker waits for completion. Both are needed for ordered shutdown.
9. **`biased` select** — Ensures cancellation checked before message processing in all event loops.
10. **`.clone().cancelled_owned()`** — For `'static` futures in spawned tasks.
11. **`Arc::try_unwrap`** — For clean shutdown of Arc-wrapped resources (only succeeds when refcount == 1).

## Sources & References

### Internal References

- Architecture: `docs/ARCHITECTURE.md` (Engine struct, component design)
- Plan: `docs/PLAN.md:187-231` (Phase 4 spec, sections 4.1-4.7)
- Phase 3 learnings: `docs/solutions/integration-issues/phase3-resilience-token-efficiency.md`
- Phase 2 learnings: `docs/solutions/integration-issues/mcp-multiplexer-http-transport-phase2.md`
- rmcp patterns: `docs/solutions/integration-issues/rmcp-sdk-integration-patterns-plug-20260303.md`
- Decisions: `docs/DECISIONS.md`
- Risks: `docs/RISKS.md`

### External References

- ratatui 0.30 docs: https://docs.rs/ratatui/0.30
- ratatui v0.30 breaking changes: https://github.com/ratatui/ratatui/blob/main/BREAKING-CHANGES.md
- ratatui async event loop: https://ratatui.rs/recipes/apps/async/
- ratatui component architecture: https://ratatui.rs/concepts/application-patterns/component-architecture/
- ratatui panic hooks: https://ratatui.rs/recipes/apps/panic-hooks/
- crossterm EventStream: https://docs.rs/crossterm/latest/crossterm/event/struct.EventStream.html
- tokio broadcast docs: https://docs.rs/tokio/latest/tokio/sync/broadcast/
- tracing-appender: https://docs.rs/tracing-appender/latest
- fs2 file locking: https://docs.rs/fs2/latest
- NO_COLOR standard: https://no-color.org/

### Related Work

- Phase 1: PR #1 (stdio proxy)
- Phase 2: PR #2 (HTTP transport)
- Phase 3: PR #3 (resilience + token efficiency)
