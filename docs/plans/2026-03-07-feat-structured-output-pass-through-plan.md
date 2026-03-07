---
title: "feat: Structured Output Pass-Through (Phase A2)"
type: feat
status: completed
date: 2026-03-07
---

# feat: Structured Output Pass-Through (Phase A2)

## Overview

Stop stripping `outputSchema` from tool definitions and verify `structuredContent` in `CallToolResult` passes through all three transports. This is a protocol correctness fix — plug currently discards information that upstream servers provide and downstream clients may need.

## Problem Statement

`strip_optional_fields()` in `plug-core/src/proxy/mod.rs:1804` explicitly sets `tool.output_schema = None`. This was originally done for token efficiency, but structured output has landed in all three official MCP SDKs (TypeScript, Python, Java) and is ranked P1-HIGH in the feature adoption analysis. Stripping it breaks clients that want to validate tool responses against the schema.

`CallToolResult.structured_content` already passes through untouched (the proxy returns the upstream result as-is), so no transport changes are needed for the response path.

## Proposed Solution

1. Remove `tool.output_schema = None;` from `strip_optional_fields()`
2. Update the doc comment on `strip_optional_fields` to reflect the change
3. Update the test `strip_optional_fields_removes_fields` to verify `output_schema` is preserved
4. Add a test verifying `structured_content` in `CallToolResult` passes through

## Acceptance Criteria

- [x] `outputSchema` from upstream tool definitions is preserved in downstream `tools/list` responses
- [x] `structuredContent` in `CallToolResult` passes through all three transports (stdio, HTTP, IPC)
- [x] Existing tests updated, no regressions
- [x] Clippy + fmt clean

## Sources & References

- Research: `docs/research/2026-03-07-mcp-feature-adoption-analysis.md` (P1-HIGH ranking)
- Code: `plug-core/src/proxy/mod.rs:1802` (`strip_optional_fields`)
- Code: `plug-core/src/proxy/mod.rs:1455` (`CallToolResult` pass-through)
