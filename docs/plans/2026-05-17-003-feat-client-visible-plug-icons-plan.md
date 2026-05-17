---
status: completed
created: 2026-05-17
title: "feat: client-visible Plug icon assets"
---

# Client-Visible Plug Icon Assets Plan

## Problem Frame

PR #57 makes Plug spec-correct for MCP icon metadata, but visible client behavior still depends on each host's UI. Codex Desktop does not currently document rendering MCP `serverInfo.icons`, and Claude Desktop's visible extension surface is centered on MCPB/Desktop Extension manifests. Plug also advertises only an SVG icon today, while the MCP icon spec requires PNG/JPEG support only for clients that render icons and treats SVG/WebP as optional.

The next change should make Plug's icon metadata maximally consumable by current clients without changing Plug's protocol identity or upstream icon pass-through behavior.

## Requirements Trace

- Keep Plug's top-level MCP `serverInfo.icons`; clients connect to Plug as the parent server.
- Add committed PNG icon assets in common square sizes so clients that do not render SVG have a spec-friendlier option.
- Advertise PNG icons before SVG in all Plug initialize paths.
- Keep the existing SVG as a source/extra icon; do not redesign the icon in this PR.
- Add tests that prove stdio/HTTP/IPC initialize responses include PNG-first icon metadata.
- Add Claude Desktop/MCPB packaging metadata that uses PNG icon fields, because MCPB manifests are the documented extension UI icon surface.
- Document the distinction between MCP runtime icons and MCPB/Desktop Extension icons.
- Do not add upstream icon byte fetching, transcoding, or cache/proxy surfaces.

## Current Patterns To Follow

- Plug's top-level implementation metadata is currently duplicated in:
  - `plug-core/src/proxy/mod.rs`
  - `plug-core/src/http/server.rs`
  - `plug/src/ipc_proxy.rs`
- The source vector icon lives at `docs/assets/plug-icon.svg`.
- Existing docs for client behavior are in:
  - `docs/CLIENT-COMPAT.md`
  - `docs/MCP-SPEC.md`
  - `README.md`
- Release/distribution scripts already live under `scripts/`; packaging helpers should follow that layout rather than adding a new build system.

## Key Technical Decisions

1. **PNG-first MCP metadata.**
   - Rationale: MCP clients that render icons must support PNG/JPEG; SVG is only recommended. PNG-first metadata improves compatibility while keeping the SVG available for clients that prefer scalable assets.

2. **Commit generated PNG assets.**
   - Rationale: icon assets are small, stable, and useful to docs, MCP metadata, and packaging. Committed assets avoid runtime conversion dependencies.

3. **Use MCPB packaging as a template/scripted artifact, not a default install path.**
   - Rationale: Plug's live install path is still cargo/crates. MCPB is specifically for Claude Desktop one-click extension surfaces, so it should be available without replacing normal installs.

4. **Do not promise Codex Desktop rendering.**
   - Rationale: public Codex docs explain MCP configuration, but do not currently document icon rendering. Plug should expose correct metadata and leave client rendering truthfully documented.

## Implementation Units

### U1: Add PNG Icon Assets

Files:

- Add: `docs/assets/plug-icon-16.png`
- Add: `docs/assets/plug-icon-32.png`
- Add: `docs/assets/plug-icon-64.png`
- Add: `docs/assets/plug-icon-128.png`
- Add: `docs/assets/plug-icon-256.png`
- Add: `docs/assets/plug-icon-512.png`
- Add: `packaging/mcpb/assets/plug-icon-16.png`
- Add: `packaging/mcpb/assets/plug-icon-32.png`
- Add: `packaging/mcpb/assets/plug-icon-64.png`
- Add: `packaging/mcpb/assets/plug-icon-128.png`
- Add: `packaging/mcpb/assets/plug-icon-256.png`
- Add: `packaging/mcpb/assets/plug-icon-512.png`

Approach:

- Generate PNGs from `docs/assets/plug-icon.svg`.
- Preserve square dimensions exactly.
- Copy the same PNGs into `packaging/mcpb/assets/` so `packaging/mcpb/manifest.json` validates in place.
- Verify dimensions with a local image metadata tool.

Tests / checks:

- Verify each PNG is readable and has the expected dimensions.

### U2: Centralize Plug Implementation Icon Metadata

Files:

- Modify: `plug-core/src/proxy/mod.rs`
- Modify: `plug-core/src/http/server.rs`
- Modify: `plug/src/ipc_proxy.rs`

Approach:

- Add constants for public icon asset URLs.
- Advertise PNG icons first: 16, 32, 64, 128, 256, 512.
- Keep SVG last with `sizes: ["any"]`.
- Avoid introducing a cross-crate dependency cycle; duplicate minimal constants only where current initialize code already duplicates implementation metadata.

Tests:

- Existing proxy `get_info_returns_correct_server_info` should assert PNG-first icons and SVG fallback.
- Existing HTTP initialize test should assert `serverInfo.icons[0]` is PNG and includes a size.
- Existing IPC initialize test should assert `serverInfo.icons[0]` is PNG and includes a size.

### U3: Add MCPB Packaging Metadata

Files:

- Add: `packaging/mcpb/manifest.json`
- Add: `scripts/build-mcpb.sh`

Approach:

- Create a manifest following MCPB manifest version `0.3`.
- Use `display_name: "Plug"`.
- Use `icon` plus `icons` fields pointing at packaged PNG assets.
- Declare the server as a binary entry point and run `plug connect`.
- Add a script that assembles a temporary MCPB directory containing:
  - `manifest.json`
  - `bin/plug`
  - PNG icon assets
- Prefer `mcpb pack` when installed; otherwise leave a clear error telling the operator how to install the MCPB CLI.

Tests:

- Script dry-run or validation mode confirms required inputs exist.
- Manifest JSON parses successfully.

### U4: Documentation

Files:

- Modify: `README.md`
- Modify: `docs/CLIENT-COMPAT.md`
- Modify: `docs/MCP-SPEC.md`

Approach:

- Explain that MCP runtime icons and MCPB/Desktop Extension icons are separate surfaces.
- State that PNG assets are advertised first for compatibility.
- Be explicit that Codex Desktop may ignore icon metadata today.
- Document how to build the Claude Desktop MCPB artifact if a user wants Plug to appear with an icon in Claude's extension UI.

Tests:

- Documentation references only files/commands added by this change.

## Sequencing

1. Generate and verify PNG assets.
2. Update initialize metadata and tests.
3. Add MCPB manifest/script and validation.
4. Update docs.
5. Run targeted tests, full workspace tests, clippy, advisory check, and CI.

## Risks

- **Initialize payload size:** embedded PNG icons avoid mutable remote asset fetches, but they add branding bytes to initialize responses. The assets are small enough for one-time initialize metadata and are not copied onto routed tool entries.
- **MCPB package completeness:** a manifest alone is not installable without a bundled binary. The script must assemble the binary and icon assets into the package layout.
- **Client rendering ambiguity:** Codex Desktop and Claude Desktop may still not render MCP runtime icons. The docs must not overpromise.
