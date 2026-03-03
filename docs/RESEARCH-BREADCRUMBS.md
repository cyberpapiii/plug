# Research Breadcrumbs

Open questions, edge cases, deeper research signals, and things that could go wrong. This is the document that ensures nothing gets missed. Every item here is a signal for deeper investigation.

---

## Critical Open Questions (Must Resolve Before Coding)

### Q1: rmcp Proxy Pattern
**Question**: Can rmcp's `ServerHandler` and `ClientHandler` coexist in one binary to form a proxy? What's the composition pattern?

**Why it matters**: fanout is fundamentally a proxy — it's an MCP server to downstream clients AND an MCP client to upstream servers. If rmcp doesn't support this cleanly, we may need to use rmcp for one side and implement the other side manually.

**Research signals**:
- Look at rmcp's examples/ directory for proxy examples
- Check if AgentGateway (Rust, Linux Foundation) uses rmcp or has its own MCP implementation
- Search for `rmcp proxy` or `rmcp client server` patterns in GitHub
- Read rmcp's `Transport` trait — can we create a custom transport that bridges client↔server?
- Check the `IntoTransport` trait for composing `(Sink, Stream)` pairs

### Q2: Lazy Schema Loading — Spec Compliance
**Question**: Does the MCP spec allow `tools/list` to omit `inputSchema`? Or is inputSchema required?

**Why it matters**: Layer 1 of our token efficiency strategy (91% reduction) depends on returning tool names without schemas. If the spec requires inputSchema, we need a different approach (maybe a custom `tools/list_summary` method, or schema truncation).

**Research signals**:
- Read the MCP spec tools section carefully — is inputSchema marked REQUIRED or OPTIONAL?
- Check what Claude Code's Tool Search actually does — does it omit schemas in the initial list?
- Look at OpenMCP's lazy loading implementation: https://www.open-mcp.org/blog/lazy-loading-input-schemas
- Check if any MCP SDK supports a "lazy" mode for tool listing
- Look at SEP-1576 (mitigating token bloat) for proposed spec changes

### Q3: Session Sharing Safety
**Question**: Is it safe for multiple downstream clients to share one upstream MCP session? Or does MCP assume 1:1 session mapping?

**Why it matters**: Our architecture proposes shared upstream sessions for efficiency (10 clients → 1 upstream connection per server). If MCP assumes session isolation (e.g., per-session subscriptions, per-session logging levels), sharing could cause bugs.

**Research signals**:
- Read the MCP spec on session semantics — is state per-session or per-request?
- Check how `logging/setLevel` works — is it per-session? If Client A sets debug, does Client B get debug logs?
- Check how `resources/subscribe` works — is the subscription per-session?
- Look at how Envoy AI Gateway handles this (they do session sharing)
- If session isolation is required, fallback: one upstream session per client (N:N instead of N:1)

### Q4: `fanout connect` — Multiple Concurrent Instances
**Question**: When two Claude Code instances both invoke `fanout connect`, how do they communicate with the running fanout daemon?

**Why it matters**: Each `fanout connect` invocation is a separate process with its own stdin/stdout. They need to communicate with the shared fanout engine that manages upstream servers.

**Research signals**:
- Option A: Each `fanout connect` is a full multiplexer instance (shared-nothing). Upstream servers are started per-instance. Simple but wasteful.
- Option B: One fanout daemon runs in the background. `fanout connect` instances connect to it via Unix socket/IPC. Efficient but adds daemon management complexity.
- Option C: First `fanout connect` becomes the leader, spawns servers. Subsequent instances connect to the leader via IPC. Leader election adds complexity.
- Study how tmux and zellij handle this (session server model).
- Study how docker CLI talks to dockerd.

### Q5: Tool Name Collision Strategy
**Question**: When prefixing is disabled and two servers define a tool with the same name, what happens?

**Why it matters**: Without prefixing, `create_issue` from GitHub and `create_issue` from Jira would collide. The user expects both to work.

**Research signals**:
- Default: prefixing ON (no collision possible)
- When prefixing OFF: detect collision at merge time, warn user, serve the first server's version
- Alternative: automatic prefixing only for colliding tools
- Check how MetaMCP handles this (namespaces)
- Check how FastMCP handles this (namespace transforms)

---

## Transport Edge Cases

### E1: stdio Buffer Deadlock
**Scenario**: Upstream server writes a large response to stdout. fanout's stdin buffer fills. Both sides block waiting for the other to read.

**Research**: How do existing MCP clients handle this? Is there a recommended buffer size? Should we use async pipe reads with configurable buffer sizes?

### E2: Streamable HTTP — POST Response vs SSE Stream Decision
**Scenario**: Client POSTs a `tools/call` request. Should fanout respond with a single JSON response or open an SSE stream?

**Research**: The spec says either is valid. What do clients actually handle? Does Claude Desktop handle SSE responses to POST? Does Cursor? The safest default is single JSON response (simpler, no stream management).

### E3: SSE Stream Termination
**Scenario**: A client has an open GET SSE stream. fanout needs to restart. How do we cleanly terminate all streams?

**Research**: The spec says server MAY close SSE connections at any time. Clients should handle reconnection. But do they? Test with each client. Send `retry` field before closing.

### E4: Last-Event-ID Replay
**Scenario**: Client reconnects with `Last-Event-ID: evt-42`. We need to replay everything after evt-42.

**Research**: How long should we buffer events for replay? Memory implications? Should we have a configurable buffer size? What if the client requests an ID we've already evicted?

### E5: HTTP and stdio Simultaneously
**Scenario**: fanout serves both HTTP (for Gemini CLI) and stdio (for Claude Code) simultaneously. An upstream server sends a `list_changed` notification.

**Research**: The notification must reach ALL connected clients regardless of transport. Verify our event bus → client session broadcast handles mixed transports correctly.

---

## Concurrency Edge Cases

### E6: Concurrent tools/call to the Same Server
**Scenario**: Client A and Client B both call different tools on the same upstream server at the same time.

**Research**: The upstream server receives two requests on the same stdio pipe (or HTTP connection). Are JSON-RPC request IDs guaranteed unique? Our ID remapping must ensure uniqueness on the upstream side. Use a monotonically increasing counter per upstream session.

### E7: tools/list During tools/call
**Scenario**: A `list_changed` notification triggers a `tools/list` re-fan-out. Meanwhile, a `tools/call` is in flight to the same server.

**Research**: Are `tools/list` and `tools/call` safe to interleave on the same connection? JSON-RPC is request-response, so IDs should disambiguate. But does the upstream server handle concurrent requests correctly? (Many stdio servers are single-threaded.)

### E8: Config Reload During Active Calls
**Scenario**: User edits config.toml, removing a server. fanout hot-reloads. But there's an active tool call to that server.

**Research**: Don't kill the server until all in-flight requests complete (or timeout). Mark it as "draining" — accept no new requests, wait for in-flight to complete, then shutdown.

---

## Client-Specific Edge Cases

### E9: Cursor's Undocumented Behavior
**Scenario**: Cursor's 40-tool limit is documented in forums but not in official docs. What if it changes?

**Research**: Monitor Cursor's changelog and forum. Make tool limits configurable in fanout's config (not hardcoded). Default to 40 but allow override.

### E10: Gemini CLI's Sequential Discovery
**Scenario**: Gemini calls `list_prompts`, waits for response, then calls `tools/list`. If prompts/list takes 5 seconds, tools discovery doesn't start until second 5 of a 60-second timeout.

**Research**: Pre-cache prompts/list at startup so it's always instant. Never block prompts/list on upstream servers. Return empty if not yet ready.

### E11: Codex's resources/list Sensitivity
**Scenario**: Codex calls `resources/list` before `tools/list`. If resources/list errors, Codex marks the entire server as unavailable.

**Research**: ALWAYS return `{resources: []}` for resources/list, even if upstream servers error. Never propagate an error for this method.

### E12: Client Identification Failure
**Scenario**: A client sends `clientInfo.name: "My Custom Agent"`. We can't detect the client type.

**Research**: Default to no tool limit, full tool list, standard behavior. Log the unknown client name for future identification. Allow config-based client profile override.

---

## Token Efficiency Research

### E13: Schema Size Distribution
**Question**: How large are real-world MCP tool schemas? What's the average token count per tool?

**Research**: Collect schema sizes from popular MCP servers (GitHub, Notion, Postgres, Filesystem, Brave, Slack). Calculate token counts. This informs whether lazy loading is worth the complexity.

### E14: Tool Search Relevance
**Question**: When a client has 100+ tools and we provide a `search_tools` meta-tool, how do we rank results?

**Research**:
- BM25 text ranking (used by mcpproxy-go, claims 43% accuracy improvement)
- Simple substring/prefix matching
- Category-based filtering
- Should we embed tool descriptions for semantic search? (Probably overkill for local use)

### E15: Claude Code's Tool Search Protocol
**Question**: How exactly does Claude Code's built-in Tool Search work? Is it an MCP extension, or does it do client-side filtering?

**Research**: Check Claude Code's documentation and source for Tool Search. If it's client-side, our lazy schema loading aligns. If it's an MCP extension, we should implement the same protocol.

---

## Architecture Research

### E16: Daemon vs Embedded Architecture
**Question**: Should `fanout connect` run a full embedded multiplexer, or should it connect to a background daemon?

**Trade-offs**:
| Aspect | Embedded (each connect is independent) | Daemon (connect talks to background process) |
|--------|----------------------------------------|----------------------------------------------|
| Simplicity | Simpler (no IPC) | More complex (need daemon management) |
| Resource usage | N copies of upstream connections | One copy, shared |
| Failure isolation | One crash doesn't affect others | Daemon crash kills everything |
| First-run UX | Just works | Need daemon auto-start |
| Headless mode | Not applicable | Natural fit |

**Research**: Study how MCP-proxy (sparfenyuk) handles this. Study tmux session model. Study Docker client/daemon split. Recommendation: Start with embedded (Phase 1), migrate to daemon (Phase 4) when TUI is added.

### E17: Process Group Management
**Question**: When fanout exits (graceful or crash), how do we ensure all child MCP server processes are killed?

**Research**:
- Use process groups (setsid) so SIGTERM kills the group
- Register a SIGINT/SIGTERM handler that kills children
- Use `tokio::process::Command` kill-on-drop behavior
- What happens if fanout is SIGKILLed? Children become orphans. Use a PID file and cleanup on next start?
- Study how VS Code handles extension host process cleanup

### E18: Windows Support
**Question**: How different is the Windows implementation?

**Research**:
- No Unix signals (SIGTERM, SIGHUP) — use Windows equivalents
- No Unix sockets — use named pipes for IPC
- No setsid/process groups — use Job Objects for child process management
- `.localhost` DNS resolution may be inconsistent on older Windows
- crossterm handles terminal differences
- tokio handles async I/O differences
- Test with Windows Terminal + PowerShell

---

## Security Research

### E19: DNS Rebinding
**Question**: The MCP spec requires Origin header validation. How exactly should we implement this?

**Research**: Read the spec's security section. What Origins are valid for localhost? What about `.localhost` subdomains? Should we accept `null` Origin (some clients send this)?

### E20: Config File Permissions
**Question**: The config file may contain env var NAMES (not values) and server commands. Should we warn if the file is world-readable?

**Research**: Check file permissions on config load. Warn (but don't block) if permissions are too open. Document recommended permissions (600 or 644).

### E21: Upstream Server Trust
**Question**: We pass through tool annotations from upstream servers. What if a malicious server sets `destructiveHint: false` on a destructive tool?

**Research**: The MCP spec says clients MUST treat annotations from untrusted servers as advisory only. fanout should NOT override annotations by default but should allow per-server annotation overrides in config.

---

## Performance Research

### E22: Benchmark Targets
**Question**: What are the right performance targets?

**Research**:
- Tool call overhead (no network): < 5ms (routing + ID remapping)
- `tools/list` from cache: < 10ms
- `tools/list` full fan-out (5 servers): < 500ms (limited by slowest server)
- Cold start to ready: < 1 second (all servers started + initialized)
- Memory baseline: < 50 MB (no servers connected)
- Memory per server: < 10 MB (including tool cache)
- Memory per client session: < 5 MB
- Binary size: < 10 MB (release, stripped)

### E23: Stress Testing
**Question**: What happens under extreme load?

**Research**: Test with 20 concurrent clients, 10 upstream servers, 500 total tools, rapid tool calls. Where does it break? What's the bottleneck — CPU (JSON parsing), memory (tool cache), I/O (upstream connections), or something else?

---

## Future-Proofing Research

### E24: Stateless MCP (June 2026)
**Question**: How much of our session management becomes obsolete with stateless MCP?

**Research**: Read SEP-1442 carefully. If sessions become optional, our SessionManager may be simplified but not removed (we still need to track client connections internally). The key change: clients may skip initialization and send requests directly. Design sessions as a layer, not a requirement.

### E25: Server Cards
**Question**: What should our `/.well-known/mcp.json` contain?

**Research**: The spec for Server Cards isn't finalized. Draft a reasonable format based on the SEP. Include: name, version, description, tool count, server list, supported transports, auth requirements.

### E26: A2A Protocol
**Question**: Should fanout also multiplex A2A (Agent-to-Agent) protocol?

**Research**: AgentGateway and IBM ContextForge both support A2A. Is A2A gaining traction? Is it worth supporting? For now, MCP-only. Revisit when A2A stabilizes.

---

## Naming and Branding Research

### E27: "fanout" Availability
**Question**: Is "fanout" truly available everywhere we need it?

**Research**:
- [ ] crates.io: check `fanout` availability
- [ ] npm: check (for potential future JS SDK)
- [ ] PyPI: check
- [ ] Homebrew: check for conflicts
- [ ] GitHub: check `fanout` and `fanout-mcp`
- [ ] Domain: `fanout.dev`, `getfanout.dev`, `fanout.sh`
- [ ] Twitter/X handle

If "fanout" is taken anywhere critical, alternatives: `hitch`, `prong`, `muxd`, `mcpipe`

---

## Testing Strategy Research

### E28: Integration Testing Without Real Servers
**Question**: How do we integration-test fanout without running real MCP servers?

**Research**:
- Mock MCP servers (simple stdio processes that respond to JSON-RPC)
- rmcp's built-in test utilities (if any)
- Record/replay of real MCP sessions
- Property-based testing for JSON-RPC message handling (proptest crate)

### E29: Client Compatibility Testing
**Question**: How do we verify fanout works with all 10+ clients?

**Research**:
- Manual testing matrix (time-consuming but necessary)
- Automated tests that simulate each client's initialization behavior
- Claude Code's MCP inspector tool
- mcptools (Go CLI inspector) for protocol validation
- CI matrix with simulated client behaviors

---

## Documentation Signals

Things that MUST be documented for users:

1. How to connect each specific client (with exact config snippets)
2. What happens when tool limits apply (which tools get dropped, how to customize)
3. How to add/remove servers without restart
4. How to debug "tools not showing up" problems
5. How to run headless on a server
6. How to migrate from existing MCP Router / MetaMCP / mcp-proxy setups
7. What `fanout doctor` checks for
8. How `.localhost` routing works (and why it doesn't need /etc/hosts)
