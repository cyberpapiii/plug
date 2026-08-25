# Modern MCP Peer Certification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable each modern MCP direction only after deterministic conformance and real-peer evidence proves it works without regressing legacy traffic.

**Architecture:** Keep protocol-era gates independent. A versioned compatibility inventory records requested/selected protocols and feature results for each client/upstream. Isolated fixtures test all four legacy/modern combinations; live peers are enabled through explicit per-peer policy rather than one global cutover.

**Tech Stack:** Rust integration tests, official MCP conformance tools, Plug protocol logs, installed Claude/Cursor/Codex clients, representative upstream servers.

**Spec:** `docs/superpowers/specs/2026-08-25-plug-macos-app-design.md`

## Global Constraints

- Legacy support remains available while any installed or supported peer needs it.
- Never infer protocol support from version strings alone.
- Do not advertise a capability Plug cannot faithfully translate end to end.
- Modern downstream and upstream gates remain independently reversible.
- Real traffic proof follows isolated deterministic tests; it never replaces them.

---

### Task 1: Build the four-quadrant certification harness

**Files:**
- Modify: `scripts/check-mcp-conformance.sh`
- Create: `scripts/certify-mcp-peer.sh`
- Modify: `docs/testing/mcp-compatibility-inventory.tsv`
- Modify: `plug-core/tests/integration_tests.rs`

**Interfaces:**
- Produces: machine-readable certification JSON and inventory rows for legacy/legacy, legacy/modern, modern/legacy, modern/modern.

- [ ] **Step 1: Add failing fixtures for every quadrant**

Cover initialize/discover, tools/resources/prompts, completion, tasks, subscriptions, cancellation, OAuth, reverse requests, resultType, cache metadata, and protocol/header consistency. Expected unsupported bridges must return a deterministic suppressed-capability result, not hang.

- [ ] **Step 2: Run fixtures with both gates off and capture the baseline**

Run: `cargo test -p plug-core --test integration_tests modern_ -- --nocapture`

Expected: legacy baseline passes; gated modern cases report disabled.

- [ ] **Step 3: Implement the certification runner**

The script takes peer name, direction, command/URL, expected protocol, and output path. It creates temporary config/state, runs the exact feature matrix, saves redacted JSON, and exits nonzero on any required failure.

- [ ] **Step 4: Commit**

```bash
git add scripts/check-mcp-conformance.sh scripts/certify-mcp-peer.sh docs/testing/mcp-compatibility-inventory.tsv plug-core/tests/integration_tests.rs
git commit -m "test: add modern MCP peer certification matrix"
```

### Task 2: Certify installed downstream clients

**Files:**
- Modify: `docs/testing/mcp-compatibility-inventory.tsv`
- Create: `docs/testing/certifications/README.md`

**Interfaces:**
- Produces: verified maximum protocol and feature evidence for Claude Desktop, Claude Code, Cursor, and Codex.

- [ ] **Step 1: Record installed binary versions and fresh requested/selected protocol logs**

Restart each client connection after enabling modern downstream only in an isolated Plug instance. Do not use old sessions or binary string scans as positive evidence.

- [ ] **Step 2: Run the matrix per client**

Require list/call tool, list/read resource, list/get prompt, completion, cancellation, and any advertised modern features. Record exact negotiated protocol and failures.

- [ ] **Step 3: Classify each peer**

Use `certified_modern`, `certified_legacy`, or `unknown`; never optimistic partial labels. Commit only redacted evidence summaries.

- [ ] **Step 4: Commit**

```bash
git add docs/testing/mcp-compatibility-inventory.tsv docs/testing/certifications
git commit -m "test: certify installed downstream MCP clients"
```

### Task 3: Certify representative upstream servers

**Files:**
- Modify: `docs/testing/mcp-compatibility-inventory.tsv`
- Create: `docs/testing/certifications/upstreams.md`

**Interfaces:**
- Produces: per-upstream era and feature evidence.

- [ ] **Step 1: Select one local fixture and at least two real remote upstreams**

Include one known modern reference server and one currently used authenticated service. Use temporary credentials/state where possible and never commit tokens.

- [ ] **Step 2: Run the upstream-direction matrix**

Require catalog, call/read/get, cancellation, subscriptions, OAuth refresh, reconnect, tasks/resultType only when advertised, and cache/metadata preservation.

- [ ] **Step 3: Commit redacted classifications**

```bash
git add docs/testing/mcp-compatibility-inventory.tsv docs/testing/certifications/upstreams.md
git commit -m "test: certify modern MCP upstreams"
```

### Task 4: Add per-peer protocol policy and enable only certified directions

**Files:**
- Modify: `plug-core/src/config/mod.rs`
- Modify: `plug-core/src/client_detect.rs`
- Modify: `plug-core/src/server/mod.rs`
- Modify: `plug-core/src/http/server.rs`
- Modify: `plug-core/src/proxy/handler.rs`
- Modify: `docs/CLIENT-COMPAT.md`

**Interfaces:**
- Produces: `ProtocolPolicy::{Auto,Legacy,Modern}` per client/upstream with `Auto` selecting modern only from certified evidence/runtime negotiation.

- [ ] **Step 1: Write policy-selection and fallback tests**

Prove certified peers select modern, unknown peers select legacy, explicit legacy always wins, explicit modern fails clearly when negotiation is unavailable, and one peer's choice cannot change another's.

- [ ] **Step 2: Implement per-peer policy without changing global defaults**

Keep global gates as emergency kill switches. Add optional client/server policy maps; `Auto` is conservative and requires a compatible requested protocol plus completed feature handshake.

- [ ] **Step 3: Enable only peers certified in Tasks 2 and 3**

Update owner config through the daemon operator API, restart/reconnect only affected peers, and re-run their exact certification. Roll back a peer independently on failure.

- [ ] **Step 4: Run full legacy and modern gates**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo +1.88.0 check --workspace
cargo deny check advisories
scripts/check-todo-status.sh
scripts/check-mcp-conformance.sh
```

- [ ] **Step 5: Update truth docs and commit**

```bash
git add plug-core docs/CLIENT-COMPAT.md docs/PROJECT-STATE-SNAPSHOT.md docs/PLAN.md
git commit -m "feat: enable modern MCP for certified peers"
```
