# Risk Register

Risks identified during pre-implementation research, ranked by likelihood × impact.

---

## Critical Risks

### R1: N×M Session Scaling Under Load
**Likelihood**: Medium | **Impact**: High
**Description**: With N clients × M servers = N×M upstream sessions, a user with 10 clients and 10 servers has 100 sessions. Each session involves an initialize handshake and ongoing connection.
**Mitigation**:
- Phase 1 embedded mode limits to actual concurrent clients (typically 3-5)
- Phase 4 daemon can implement session pooling with per-client state isolation
- Lazy session creation: only create upstream session when client first accesses that server
- Session timeout: close idle upstream sessions after 5 min

### R2: rmcp Breaking API Changes
**Likelihood**: Medium | **Impact**: High
**Description**: rmcp just hit 1.0.0, but 0.12→0.16 had significant migrations. Future versions may break our proxy pattern.
**Mitigation**:
- Pin to `rmcp = "1.0"` (semver should protect us for 1.x)
- Abstract rmcp types behind internal traits where practical
- Monitor rmcp releases and test against nightly
- Worst case: fork rmcp at our pinned version

### R3: MCP Spec June 2026 Stateless Changes
**Likelihood**: High | **Impact**: Medium
**Description**: SEP-1442 proposes removing mandatory initialization, making sessions optional, removing `logging/setLevel`. This changes fundamental assumptions.
**Mitigation**:
- Session management behind `trait SessionStore`
- Support both stateful (current) and stateless (future) via config flag
- Don't deeply couple to initialization handshake
- Per-request context passing as internal model

---

## High Risks

### R4: Upstream Server Compatibility
**Likelihood**: Medium | **Impact**: Medium
**Description**: Many MCP servers are single-threaded stdio processes. Concurrent requests (multiple clients calling same server) may cause issues.
**Mitigation**:
- Per-server concurrency semaphore (default: 1 for stdio, configurable)
- Request queuing for single-threaded servers
- Test with popular servers: github, filesystem, postgres, notion

### R5: Name "plug" Conflicts
**Likelihood**: High | **Impact**: Low
**Description**: Homebrew cask "plug" is taken (music player). plug.dev taken. GitHub org taken.
**Mitigation**:
- Use `plug-mcp` for GitHub repo and Homebrew tap
- Binary name remains `plug` (no conflict at binary level)
- Check crates.io `plug` availability; use `plug-mcp` if taken
- Consider alternative: `fanout`, `hitch`, `muxd`

### R6: tui-logger Incompatibility
**Likelihood**: High | **Impact**: Low
**Description**: tui-logger pins ratatui 0.29, conflicting with our target of ratatui 0.30.
**Mitigation**:
- Build custom TUI log widget (~100-200 LOC)
- Use ratatui's built-in tracing recipe
- Subscribe to tracing events directly

---

## Medium Risks

### R7: Gemini CLI 60s Timeout
**Likelihood**: Medium | **Impact**: Medium
**Description**: Gemini CLI has hardcoded 60s timeout for MCP discovery that ignores config. If startup + fan-out takes >60s, Gemini marks us as unavailable.
**Mitigation**:
- Pre-cache `prompts/list` and `tools/list` at startup
- Return cached/empty responses instantly, update asynchronously
- `prompts/list` MUST respond in <100ms

### R8: stdio Buffer Deadlock
**Likelihood**: Low | **Impact**: High
**Description**: Large MCP responses could fill stdio buffers, causing both sides to block.
**Mitigation**:
- Async pipe reads with configurable buffer sizes
- tokio's async I/O should handle this, but test with large payloads
- Set buffer size to 1MB+ for stdout/stdin pipes

### R9: Config File Security
**Likelihood**: Low | **Impact**: Medium
**Description**: Config contains server commands and env var references. World-readable config could expose sensitive info.
**Mitigation**:
- Warn on config load if permissions > 644
- Never log env var values, only names
- Document recommended permissions (600)

### R10: Orphaned Child Processes
**Likelihood**: Medium | **Impact**: Medium
**Description**: If plug is SIGKILLed, child MCP server processes become orphans.
**Mitigation**:
- Process groups (setsid) for child processes
- SIGINT/SIGTERM handler kills children
- PID file + cleanup on next start
- State file tracking child PIDs

---

## Low Risks

### R11: inputSchema Token Overhead
**Likelihood**: Medium | **Impact**: Low
**Description**: Since we can't omit inputSchema, token usage is higher than planned.
**Mitigation**: Claude Code and Cursor handle this client-side. $ref dedup helps.

### R12: Windows Compatibility
**Likelihood**: Low | **Impact**: Medium
**Description**: No Unix signals, no Unix sockets, no process groups on Windows.
**Mitigation**: Windows uses named pipes for IPC, Job Objects for process groups, crossterm handles terminal differences. Phase 5 scope.

### R13: AgentGateway Competitive Pressure
**Likelihood**: Medium | **Impact**: Low
**Description**: AgentGateway is Rust, Linux Foundation-backed, and implements similar features.
**Mitigation**: Our differentiation is simplicity (personal tool vs enterprise platform), TUI, client-awareness, and zero-friction install. AgentGateway targets K8s/enterprise.
