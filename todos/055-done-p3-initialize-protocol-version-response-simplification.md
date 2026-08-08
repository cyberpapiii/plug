---
status: done
priority: p3
issue_id: "055"
tags: [code-review, http, protocol, simplicity, testing]
dependencies: []
---

# Simplify downstream HTTP initialize protocol-version override

## Problem Statement

The current local fix for the downstream HTTP `initialize` response overrides
`result.protocolVersion` by serializing a `ServerJsonRpcMessage` to a
`serde_json::Value`, mutating the nested field, and then returning that value
through a second response helper path.

This is correct, but it adds duplicate response-construction helpers for a
single special case and makes the initialize path harder to follow than
necessary.

## Findings

- Identified during CE review of the local downstream HTTP connector
  compatibility fix
- `plug-core/src/http/server.rs` now has:
  - `json_response(...)`
  - `json_value_response(...)`
  - `json_value_response_with_session(...)`
- The duplication exists only to support the one initialize-body override
- Regression coverage is split across:
  - one test checking the initialize response body
  - another test checking the initialize response header

## Proposed Solutions

### Option A: Keep special-case override local (Recommended)

Retain the body override in the initialize branch, but extract only the
minimal shared response-building primitive needed so header/body assembly does
not live in parallel helper stacks.

**Pros:** Small change, clearer intent, less helper duplication  
**Cons:** Still preserves a one-off initialize special case  
**Effort:** Small  
**Risk:** Low

### Option B: Introduce a single generic JSON response builder

Refactor the HTTP response helpers so both `ServerJsonRpcMessage` responses and
ad hoc JSON-value responses go through one shared implementation.

**Pros:** Removes duplication completely  
**Cons:** Slightly larger refactor than the bugfix itself needs  
**Effort:** Small-Medium  
**Risk:** Low

## Acceptance Criteria

- [x] The initialize response override no longer requires parallel helper
      functions with duplicated header/body setup
- [x] One regression test asserts both:
      - `MCP-Protocol-Version` header
      - `result.protocolVersion` body field
      on the same initialize response

## Work Log

- 2026-03-10: Added from CE review of the downstream HTTP connector
  compatibility fix. No functional blocker; follow-up only.

### 2026-08-08 - Closed

**By:** Claude Fable 5

**Criterion 1 was already satisfied before this pass** — resolved incidentally by
the dual-era protocol work rather than by a dedicated refactor. `json_value_response`
and `json_value_response_with_session` no longer exist; `plug-core/src/http/server.rs`
now has a single response path, `json_response_for_era`, where the legacy body
override happens inline via `crate::protocol::rewrite_legacy_response(&mut value, false)`.
`json_response` and `json_response_with_session` are thin wrappers over it, not a
parallel helper stack. This is effectively Option B, reached through the era work.

**Criterion 2 was still unmet** — the header and body assertions were split across
`post_initialize_returns_session_id` (header only) and
`initialize_response_contains_server_info` (body only), so nothing pinned the two
to the *same* response. That pairing is the actual regression from the original bug
report: the remote Claude connector showed no tools when the header and body
advertised different versions. Added the header assertion to
`initialize_response_contains_server_info` and replaced the hardcoded `"2025-11-25"`
body literal with the `PROTOCOL_VERSION` constant so the two can no longer drift apart
in the test itself.

**Verification:** `cargo test -p plug-core initialize` (3 passed).

## Resources

- `plug-core/src/http/server.rs`
- `docs/bug-reports/claude-remote-mcp-no-tools-when-initialize-body-advertises-2025-06-18.md`
