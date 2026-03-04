---
status: pending
priority: p2
issue_id: "013"
tags: [code-review, performance]
dependencies: []
---

# Arc<str> to String conversion defeats O(1) clone optimization

## Problem Statement

`ToolCallStarted` handler converts `Arc<str>` to `String` when creating `ActivityEntry`, defeating the purpose of using `Arc<str>` in `EngineEvent`. Also, `ClientConnected`/`ClientDisconnected` use `String` instead of `Arc<str>`.

## Findings

- **Source**: performance-review
- **Locations**:
  - `app.rs:488-496` — `server_id.to_string()` and `tool_name.to_string()`
  - `engine.rs:62-67` — `session_id: String` in ClientConnected/Disconnected

## Acceptance Criteria

- [ ] ActivityEntry fields use `Arc<str>` instead of `String`
- [ ] ClientConnected/ClientDisconnected use `Arc<str>` for session_id
