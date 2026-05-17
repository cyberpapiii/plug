---
status: completed
created: 2026-05-17
title: "feat: upstream MCP icon pass-through"
---

# Upstream MCP Icon Pass-Through Plan

## Problem Frame

Plug now advertises its own MCP `serverInfo.icons`, but it drops upstream server-level icons after the upstream initialize handshake. That means clients connected through Plug can see Plug as the parent server, but cannot render source-specific icons for routed tools such as `Imessage__send` even when the upstream server advertises `serverInfo.icons`.

The intended behavior is multiplexor-correct: Plug keeps its own top-level icon in downstream initialize responses, preserves upstream source identity separately, and surfaces upstream icons on routed inventory/tool metadata where the current MCP spec supports icons.

## Requirements Trace

- Preserve Plug's own top-level `serverInfo.icons`; do not replace it with any single upstream icon.
- Capture upstream `serverInfo` metadata during initialize, including `name`, `title`, `version`, `description`, `websiteUrl`, and `icons`.
- Normalize and filter icon metadata using the current MCP icon shape:
  - `src` must be HTTPS or a bounded `data:` URI.
  - `mimeType` should be normalized when present or inferable.
  - `sizes` should remain valid `WxH` values or `any`.
  - `theme` is preserved when valid.
  - supported untrusted upstream formats are PNG/JPEG/WebP; SVG stays reserved for Plug-owned trusted branding because many MCP clients render icons directly.
- Expose upstream icons in operator inventory (`plug servers --output json`, tool inventory source metadata where useful).
- Surface source icons onto routed `tools/list` entries only when a tool has no explicit icon of its own.
- Preserve any upstream tool-level icon already declared by the tool.
- Avoid token and payload blowups from large base64 icons by bounding count and data URI size.
- Add regression coverage for initialize capture, routed tool icon inheritance, tool icon preservation, unsafe icon rejection, operator JSON fields, and top-level Plug icon stability.

## Scope Boundaries

- This plan does not create a new Plug visual design or replace `docs/assets/plug-icon.svg`.
- This plan does not add icon image transcoding, raster resizing, or a proxied icon byte cache. Those would require image decoding and new storage/HTTP surfaces; the first pass should normalize metadata and bound unsafe inputs.
- This plan does not invent a Plug-specific public extension when standard MCP `icons` fields exist.
- This plan does not change routed tool names, titles, or capability synthesis.

## Current Patterns To Follow

- Top-level Plug implementation metadata is centralized for stdio in `plug-core/src/proxy/mod.rs` and duplicated for HTTP/IPC initialize paths in `plug-core/src/http/server.rs` and `plug/src/ipc_proxy.rs`.
- Upstream connection finalization already reads `client.peer().peer_info()` in `plug-core/src/server/mod.rs`; that is the correct capture point for server icons.
- Tool routing and display metadata are assembled in `ToolRouter::refresh_tools()` in `plug-core/src/proxy/mod.rs`.
- Operator tool inventory already carries source/trust/risk metadata through `plug-core/src/ipc.rs`, `plug/src/daemon.rs`, and `plug/src/views/tools.rs`.
- Server operator inventory is produced from `ServerStatus` through `ServerManager::server_statuses()` and `plug/src/views/servers.rs`.

## Key Technical Decisions

1. **Store sanitized upstream implementation metadata, not raw initialize payloads.**
   - Rationale: icons are untrusted metadata and can carry unsafe schemes or giant data payloads. Keeping only a normalized subset reduces downstream risk.

2. **Use upstream server icon as a fallback for routed tools only.**
   - Rationale: a tool's own `icons` field is more specific than the server icon. Plug should not overwrite explicit upstream tool branding.

3. **Expose icon metadata additively.**
   - Rationale: adding optional `icons` and `upstream` fields to JSON inventory is backward-compatible for current consumers.

4. **Normalize metadata now; defer byte conversion/resizing.**
   - Rationale: the MCP wire format is metadata-based. Actual image conversion would require fetching remote bytes or decoding data URIs, which expands network, security, cache, and failure behavior beyond this feature.

## Implementation Units

### U1: Add Icon Normalization

Files:

- Create: `plug-core/src/icons.rs`
- Modify: `plug-core/src/lib.rs`
- Test: `plug-core/src/icons.rs`

Approach:

- Add constants for maximum icons per surface and maximum data URI length.
- Accept only `https://` and bounded `data:` sources.
- Reject `http:`, `file:`, `javascript:`, `ftp:`, `ws:`, empty sources, and oversized data URIs.
- Normalize MIME aliases such as `image/jpg` to `image/jpeg`.
- Infer MIME type from `data:image/...;base64,` or simple URL extensions when absent.
- Keep only MCP-friendly untrusted upstream MIME types: `image/png`, `image/jpeg`, and `image/webp`.
- Filter `sizes` to `any` or positive `WIDTHxHEIGHT`; de-duplicate them.

Test Scenarios:

- HTTPS PNG with `64x64` survives unchanged.
- `image/jpg` normalizes to `image/jpeg`.
- SVG with no size gains `any`.
- Unsafe URI schemes are rejected.
- Oversized `data:` icons are rejected.
- Invalid sizes are dropped without rejecting the icon.

### U2: Persist Upstream Implementation Metadata

Files:

- Modify: `plug-core/src/server/mod.rs`
- Modify: `plug-core/src/types.rs`
- Test: `plug-core/src/server/mod.rs`

Approach:

- Add a serializable upstream metadata struct containing server `name`, `title`, `version`, `description`, `website_url`, and normalized `icons`.
- Store it on `UpstreamServer` during every successful upstream connection finalization.
- Include the metadata on `ServerStatus` for live server inventory.
- For failed/auth-required configured servers, omit metadata rather than inventing it from config.

Test Scenarios:

- A mock upstream with `serverInfo.icons` produces `ServerStatus.upstream.icons`.
- Unsafe upstream icons do not appear in `ServerStatus`.
- Existing health/auth status behavior is unchanged when metadata is absent.

### U3: Inherit Upstream Icons Onto Routed Tools

Files:

- Modify: `plug-core/src/proxy/mod.rs`
- Test: `plug-core/src/proxy/mod.rs`

Approach:

- Teach `ToolRouter::refresh_tools()` to obtain upstream metadata/icons for each server.
- When building each routed tool, preserve `tool.icons` if present.
- If `tool.icons` is absent or empty, set it from the normalized upstream server icon list.
- Keep this before `strip_optional_fields()` and update comments to include icons as preserved display metadata.

Test Scenarios:

- Routed tool with no icon inherits its source server icon.
- Routed tool with explicit icon keeps its own icon.
- Routed tool from a server with no valid icons has no icons.
- Plug top-level initialize metadata still uses Plug's icon.

### U4: Extend Operator IPC And CLI Inventory

Files:

- Modify: `plug-core/src/ipc.rs`
- Modify: `plug/src/daemon.rs`
- Modify: `plug/src/views/servers.rs`
- Modify: `plug/src/views/tools.rs`
- Test: existing unit tests in touched files or new targeted tests where local patterns exist

Approach:

- Add optional upstream metadata/icons to `IpcToolInfo.source` or a dedicated additive field if source config and upstream identity should stay separate.
- Add `upstream` metadata to server JSON output.
- Add icon metadata to tool JSON output so source-specific icons are visible without reading raw `tools/list`.
- Keep text output concise; do not print base64 data URI bodies in terminal tables.

Test Scenarios:

- `plug servers --output json` includes upstream icon metadata for live servers.
- `plug tools --output json` includes routed tool icons or source upstream icons.
- Text views do not dump large data URI contents.

### U5: Documentation And Verification

Files:

- Modify: `README.md`
- Modify: `docs/MCP-SPEC.md`
- Modify as needed: `docs/CLIENT-COMPAT.md`

Approach:

- Document that Plug has its own top-level icon and passes upstream source icons through on routed tools where clients support MCP tool icons.
- Document accepted icon formats and safety/size normalization.
- Mention that Plug does metadata normalization, not image byte resizing/conversion.

Verification:

- `cargo test -p plug-core icons`
- `cargo test -p plug-core proxy::`
- `cargo test --workspace -- --test-threads=1`
- `cargo clippy --workspace -- -D warnings`
- Manual check against the local iMessage upstream if available: direct initialize shows iMessage icons, and Plug-routed `tools/list` includes iMessage icons on `Imessage__*` tools.

## Risks

- Some clients may ignore tool-level icons even when Plug sends them. That is acceptable; Plug should still emit spec-compliant metadata.
- Large upstream data URIs can bloat every `tools/list`. The normalization unit must cap data URI size and icon count before propagation.
- Fetching and transcoding remote icons would create SSRF/cache/security concerns. This pass intentionally avoids fetching remote icon bytes.

## Assumptions

- The current stable MCP icon contract is the 2025-11-25 schema: `Icon { src, mimeType?, sizes?, theme? }`.
- Tool-level icon pass-through is the best current surface for per-upstream identity in a multiplexor because initialize has only one top-level `serverInfo`.
