---
review_agents: [code-simplicity-reviewer, security-sentinel, performance-oracle, architecture-strategist]
plan_review_agents: [code-simplicity-reviewer, architecture-strategist]
---

# Review Context

- Rust project with `#![forbid(unsafe_code)]` — no unsafe blocks allowed
- Uses rmcp 1.0.0 SDK for MCP protocol
- Concurrency model: ArcSwap for immutable snapshots, DashMap for mutable state, tokio::sync primitives
- All string fields in broadcast events use `Arc<str>` for O(1) clone
- Config via Figment with TOML
- IPC uses length-prefixed JSON over Unix sockets
- Auth tokens use constant-time comparison (subtle crate)
