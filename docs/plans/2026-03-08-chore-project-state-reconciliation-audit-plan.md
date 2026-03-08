# Project State Reconciliation Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Reconcile the true current state of the project across code, tests, branches, worktrees, PRs, plans, roadmaps, todos, and audit docs, then publish a single canonical state snapshot and retire or rewrite any inaccurate documentation.

**Architecture:** Treat `main` as the only source of truth for current implementation state. Treat feature branches and worktrees as candidate future state only. Convert every document into a claim source, verify those claims against code and tests, then emit one canonical truth snapshot plus a doc reconciliation report and governance rules that prevent future drift.

**Tech Stack:** Git, GitHub CLI, ripgrep, Markdown docs in `docs/`, Rust code/tests, existing audit docs and plans

---

## Non-Negotiable Rules

These rules apply to every task in this plan:

- `main` is the only source of truth for "implemented now"
- branch/worktree code may only be labeled `exists off-main`
- docs are claims, never evidence
- PR summaries and agent outputs are leads, never evidence
- every claim must end in one verdict:
  - `done on main`
  - `partial on main`
  - `exists off-main`
  - `missing`
- every doc must be classified as:
  - `main truth`
  - `branch truth`
  - `plan / intended`
  - `historical`

## Final Deliverables

This plan is complete only when these files exist and are internally consistent:

- `docs/audit/BASELINE-2026-03-08.md`
- `docs/audit/CLAIM-REGISTRY-2026-03-08.md`
- `docs/audit/MAIN-TRUTH-MATRIX-2026-03-08.md`
- `docs/audit/OFF-MAIN-STATE-2026-03-08.md`
- `docs/audit/DOC-RECONCILIATION-2026-03-08.md`
- `docs/PROJECT-STATE-SNAPSHOT.md`

## Task 1: Create Audit Workspace

**Files:**
- Create: `docs/audit/.gitkeep`
- Create: `docs/audit/BASELINE-2026-03-08.md`

**Step 1: Create the audit directory**

Run:

```bash
mkdir -p docs/audit
touch docs/audit/.gitkeep
```

**Step 2: Create the baseline file header**

Create `docs/audit/BASELINE-2026-03-08.md` with:

```md
# Audit Baseline

Date: 2026-03-08

This file captures the exact repository state used for the reconciliation audit.
```

**Step 3: Commit the empty audit scaffolding**

Run:

```bash
git add docs/audit/.gitkeep docs/audit/BASELINE-2026-03-08.md
git commit -m "docs(audit): add audit workspace scaffolding"
```

## Task 2: Freeze Main Truth Baseline

**Files:**
- Modify: `docs/audit/BASELINE-2026-03-08.md`

**Step 1: Capture `main` SHA**

Run:

```bash
git rev-parse main
```

Record:

```md
## Main Baseline

- main_sha: `<sha>`
```

**Step 2: Capture branch topology**

Run:

```bash
git branch --all --verbose --no-abbrev
```

Add a trimmed section listing:

- local branches
- remote branches
- current branch
- which branch is `main`

**Step 3: Capture worktree topology**

Run:

```bash
git worktree list --porcelain
```

Record every worktree path, branch, and HEAD SHA.

**Step 4: Capture local dirty state**

Run:

```bash
git status --short --branch
```

Record all local changes in the current worktree.

**Step 5: Capture PR topology**

Run:

```bash
gh pr list --state all --limit 100
```

Add a compact table of roadmap-relevant PRs only.

## Task 3: Inventory Claim Sources

**Files:**
- Create: `docs/audit/CLAIM-REGISTRY-2026-03-08.md`

**Step 1: Find all claim-bearing docs**

Run:

```bash
rg -n --hidden -S "complete|completed|implemented|remaining work|not implemented|partial|done|open bug|merged|roadmap|current state" docs todos CLAUDE.md
```

**Step 2: Build the source list**

Create sections for:

- truth docs
- roadmap docs
- plan docs
- brainstorm docs with status claims
- todos
- decision docs
- audit docs

**Step 3: Create the registry format**

Use this table:

```md
| claim_id | source | section | claim_text | claimed_state | feature_area | notes |
|---|---|---|---|---|---|---|
```

**Step 4: Populate the registry**

Include at minimum:

- `docs/PLAN.md`
- `docs/ROADMAP-AUDIT-2026-03-08.md`
- `docs/plans/2026-03-07-feat-mcp-spec-compliance-roadmap-plan.md`
- all roadmap-tail / phase closeout docs
- all active `todos/*.md`

## Task 4: Define the Main Truth Matrix

**Files:**
- Create: `docs/audit/MAIN-TRUTH-MATRIX-2026-03-08.md`

**Step 1: Create the truth matrix skeleton**

Use this table:

```md
| claim_id | feature | code evidence on main | test evidence on main | verdict | doc accuracy | next action |
|---|---|---|---|---|---|---|
```

**Step 2: Group claims into audit slices**

Create sections:

- transport + auth
- notifications + logging
- tools + capability synthesis + meta-tool mode
- resources + prompts + subscriptions
- completions + structured output
- daemon + IPC + continuity
- protocol version handling
- reload / lifecycle

**Step 3: Add verdict legend**

Document:

- `done on main`
- `partial on main`
- `missing`

## Task 5: Verify Transport and Auth Claims on Main

**Files:**
- Modify: `docs/audit/MAIN-TRUTH-MATRIX-2026-03-08.md`

**Step 1: Verify downstream transport surface**

Check:

- `plug connect`
- `plug serve`
- Streamable HTTP downstream
- HTTPS
- bearer auth

Run:

```bash
rg -n "cmd_connect|cmd_serve|bind_rustls|Authorization|Bearer|build_router|POST /mcp|GET /mcp|DELETE /mcp" plug plug-core
```

**Step 2: Verify tests**

Run:

```bash
cargo test -q test_http_end_to_end_proxy_path_with_sse serve_router_supports_https
```

**Step 3: Fill matrix rows**

For each transport/auth claim, add exact file and test references.

## Task 6: Verify Notifications and Logging Claims on Main

**Files:**
- Modify: `docs/audit/MAIN-TRUTH-MATRIX-2026-03-08.md`

**Step 1: Verify notification types and fan-out**

Run:

```bash
rg -n "ToolListChanged|ResourceListChanged|PromptListChanged|Progress|Cancelled|LoggingMessage|subscribe_notifications|subscribe_logging" plug-core/src plug/src
```

**Step 2: Verify transport parity**

Specifically classify:

- stdio
- HTTP
- IPC

**Step 3: Verify tests**

Run:

```bash
cargo test -q tools_list_changed targeted_progress
```

**Step 4: Fill matrix rows**

Do not mark IPC parity as complete unless push frames exist in the daemon/client protocol.

## Task 7: Verify Resources, Prompts, Subscriptions, and Completions on Main

**Files:**
- Modify: `docs/audit/MAIN-TRUTH-MATRIX-2026-03-08.md`

**Step 1: Verify resources/prompts/catalog routing**

Run:

```bash
rg -n "list_resources|read_resource|list_resource_templates|list_prompts|get_prompt|subscribe_resource|unsubscribe_resource|complete_request|CompleteRequest" plug-core/src plug/src
```

**Step 2: Verify route-refresh lifecycle behavior**

Run:

```bash
rg -n "refresh_tools|resource_subscriptions|rebind|stale subscription|unsubscribe old resource owner" plug-core/src/proxy/mod.rs todos/039*
```

**Step 3: Verify tests**

Run:

```bash
cargo test -q subscription complete_request route_refresh
```

**Step 4: Fill matrix rows**

Split these carefully:

- resources/prompts forwarding
- subscriptions
- route refresh / todo 039
- completion per transport

## Task 8: Verify Capability Synthesis and Structured Output on Main

**Files:**
- Modify: `docs/audit/MAIN-TRUTH-MATRIX-2026-03-08.md`

**Step 1: Verify synthesized capabilities**

Run:

```bash
rg -n "synthesized_capabilities|list_changed: Some|subscribe: if any_subscribe|resources.subscribe|prompts" plug-core/src plug/src
```

**Step 2: Verify structured output evidence**

Run:

```bash
rg -n "output_schema|structured_content|resource_link|strip_optional_fields" plug-core/src plug-core/tests
```

**Step 3: Distinguish proven vs inferred**

If a behavior is present only by pass-through and not by dedicated test, mark it `partial`.

## Task 9: Verify Daemon / IPC / Continuity Claims on Main

**Files:**
- Modify: `docs/audit/MAIN-TRUTH-MATRIX-2026-03-08.md`

**Step 1: Verify daemon sharing and reconnect**

Run:

```bash
rg -n "register|session|reconnect|heartbeat|SESSION_REPLACED|Capabilities|LoggingNotification" plug/src/daemon.rs plug/src/ipc_proxy.rs plug-core/src/ipc.rs
```

**Step 2: Verify continuity scope**

Do not use broad language. Distinguish:

- daemon-backed stdio reconnect recovery
- HTTP session continuity
- full cross-transport persistence

**Step 3: Verify tests**

Run:

```bash
cargo test -q daemon_backed_proxy_recovers_after_daemon_restart test_stdio_timeout_reconnects_cleanly
```

## Task 10: Audit Off-Main State Separately

**Files:**
- Create: `docs/audit/OFF-MAIN-STATE-2026-03-08.md`

**Step 1: Select relevant branches/worktrees**

Only include branches with roadmap-relevant code or docs.

**Step 2: Diff each against main**

Run:

```bash
git diff --stat main...<branch>
git log --oneline main..<branch>
```

**Step 3: Classify each branch feature**

Use:

- `merge-ready`
- `salvageable`
- `stale`
- `superseded`
- `abandoned`

**Step 4: Never upgrade off-main work into current truth**

Record it only as candidate future state.

## Task 11: Reconcile Documents Against Truth

**Files:**
- Create: `docs/audit/DOC-RECONCILIATION-2026-03-08.md`

**Step 1: Audit every major doc**

For each doc, classify:

- `accurate`
- `stale`
- `false`
- `ambiguous`
- `historical but fine`

**Step 2: Use this template**

```md
## <doc path>

- state described:
- factual status:
- lines/sections needing rewrite:
- lines/sections needing archive marker:
- action:
```

**Step 3: Prioritize docs for rewrite**

Priority order:

1. `docs/PLAN.md`
2. canonical snapshot doc
3. roadmap plan docs
4. audit docs
5. older historical plans needing archive framing

## Task 12: Publish the Canonical Snapshot

**Files:**
- Create: `docs/PROJECT-STATE-SNAPSHOT.md`

**Step 1: Write the only doc people should use for current state**

Required sections:

- current baseline SHA
- implemented on `main`
- partial on `main`
- exists off-main
- missing
- current top-priority remaining work
- doc taxonomy and where to look next

**Step 2: Add explicit doc taxonomy**

Explain:

- truth docs track `main`
- plans describe intended future work
- branch docs describe off-main state
- historical docs are not current truth

**Step 3: Link back to audit artifacts**

Include links to all `docs/audit/*.md` outputs.

## Task 13: Add Governance Rules To Prevent Drift

**Files:**
- Modify: `CLAUDE.md`
- Modify: `docs/PLAN.md`
- Optionally create: `docs/TRUTH-RULES.md`

**Step 1: Add hard truth rules**

Required rules:

- no “complete” claims without code on `main`
- branch truth must be labeled as branch truth
- every roadmap item must link to code/test evidence or an open gap
- post-merge truth pass required for roadmap-affecting PRs

**Step 2: Add PR truth-pass checklist**

Use this checklist:

```md
- [ ] merged code exists on `main`
- [ ] truth docs updated against `main`, not branch
- [ ] branch-only claims removed or relabeled
- [ ] remaining work list revalidated
```

## Task 14: Final Verification

**Files:**
- Verify all audit docs and snapshot docs

**Step 1: Consistency grep**

Run:

```bash
rg -n "complete|completed|done|merged|remaining work|not implemented|partial" docs/audit docs/PROJECT-STATE-SNAPSHOT.md docs/PLAN.md
```

Check for contradictions.

**Step 2: Spot-check all `done on main` rows**

Randomly sample at least 10 rows from the truth matrix and re-open the referenced code/tests.

**Step 3: Final commit**

Run:

```bash
git add docs/audit docs/PROJECT-STATE-SNAPSHOT.md docs/PLAN.md CLAUDE.md
git commit -m "docs(audit): publish canonical project state snapshot"
```

## Suggested Subagent Split

Use subagents only for bounded evidence gathering, never for final truth decisions.

- Agent 1: main transport/auth audit
- Agent 2: main notifications/capabilities audit
- Agent 3: main resources/prompts/completions audit
- Agent 4: daemon/IPC/continuity audit
- Agent 5: branch/worktree inventory
- Agent 6: claim registry assembly
- Agent 7: doc reconciliation draft
- Main thread: final verdicts, doc rewrites, canonical snapshot

## Exit Criteria

Do not call this audit complete until:

- every major claim source has been inventoried
- every claim has a verdict
- every current-state doc is aligned to `main`
- branch-only work is clearly separated
- one canonical snapshot doc exists
- governance rules are in place to stop recurrence
