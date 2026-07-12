# Plan 004: Remove dead work from the catalog hot paths (pagination double-clone, per-tool refresh lookups, unused filtered views)

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md`.
>
> **Drift check (run first)**: `git diff --stat e341625..HEAD -- plug-core/src/proxy/catalog.rs plug-core/src/proxy/mod.rs`
> If either file changed since this plan was written, compare the "Current
> state" excerpts against the live code before proceeding; on a mismatch,
> treat it as a STOP condition. Another AI agent (Codex) may be working in
> this repo concurrently.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: perf
- **Planned at**: commit `e341625`, 2026-07-11

## Why this matters

Three verified sources of pure wasted work on the two hottest catalog paths
(list requests and `refresh_tools`), all output-identical to fix:

1. Every `tools/list` (and resources/templates/prompts list) deep-clones the
   ENTIRE catalog, then clones the requested page out of that copy and throws
   the first copy away. With this deployment's ~500-tool single-page setup,
   that's ~2× a full-catalog clone per list call — and list calls fire on
   every client connect and after every debounced `list_changed`.
2. `refresh_tools` resolves the upstream entry up to 3× per TOOL (ArcSwap
   load + map get + Arc clone each time) and clones the server's whole
   `tool_groups` rules Vec per tool, though all of it is per-server-constant.
3. The Windsurf (100-tool) and VS Code Copilot (128-tool) filtered views are
   built and stored on every refresh even when `tool_filter_enabled = false`
   (the operator's actual config), in which case they are provably never read.

## Current state

### 1. Pagination — `plug-core/src/proxy/catalog.rs`

The helper (`:111-128`) takes owned items and clones the page slice:

```rust
pub(crate) fn paginated_result<T: Clone, R>(
    items: Vec<T>,
    request: Option<PaginatedRequestParams>,
    build: impl FnOnce(Vec<T>, Option<String>) -> R,
) -> R {
    const PAGE_SIZE: usize = 500;
    let start = request ... .unwrap_or(0);
    let end = usize::min(start + PAGE_SIZE, items.len());
    let next_cursor = (end < items.len()).then(|| end.to_string());
    build(items[start..end].to_vec(), next_cursor)
}
```

Callers pass a full deep clone of the `Arc<Vec<T>>` contents — e.g.
`list_tools_page_for_client_session` (`:384-397`):

```rust
let tools = self.list_tools_for_client_session(client_type, session_key);
paginated_result((*tools).clone(), request, |tools, next_cursor| { ... })
```

Same `(*x).clone()` pattern in `list_resources_page` (`:607`),
`list_resource_templates_page` (`:622`), `list_prompts_page` (`:641`).

### 2. refresh_tools per-tool loop — `plug-core/src/proxy/mod.rs:1110-1183`

```rust
for (server_name, tool) in upstream_tools {
    let mut exposed_name = tool.name.to_string();
    // 1. Apply manual renames if any
    if let Some(upstream) = self.server_manager.get_upstream(&server_name) {           // lookup #1
        if let Some(new_name) = upstream.config.tool_renames.get(&exposed_name) { ... }
    }
    ...
    let tool_group_rules: Option<Vec<crate::config::ToolGroupRule>> = self
        .server_manager
        .get_upstream(&server_name)                                                     // lookup #2
        .and_then(|u| {
            if !u.config.tool_groups.is_empty() {
                Some(u.config.tool_groups.clone())                                      // Vec clone per tool
            } else if server_name == "workspace" { ... }
```

and in pass 3 (`:1236-1238`), a third per-tool lookup:

```rust
let upstream_icons = self
    .server_manager
    .get_upstream_metadata(&c.server_name)
    .and_then(|metadata| metadata.icons);
```

### 3. Unconditional filtered views — `plug-core/src/proxy/mod.rs:1338-1339`

```rust
let tools_windsurf = Arc::new(tools.iter().take(100).cloned().collect());
let tools_copilot = Arc::new(tools.iter().take(128).cloned().collect());
```

Read only when filtering is enabled — `plug-core/src/proxy/catalog.rs:542-550`:

```rust
if !self.config.tool_filter_enabled {
    return self.list_tools();
}
let snapshot = self.cache.load();
match client_type {
    ClientType::Windsurf => Arc::clone(&snapshot.tools_windsurf),
    ClientType::VSCodeCopilot => Arc::clone(&snapshot.tools_copilot),
    _ => Arc::clone(&snapshot.tools_all),
}
```

Conventions: the proxy's tests live in `plug-core/src/proxy/tests.rs`
(69 tests) plus inline modules; the cross-transport parity matrix in the
workspace test suite pins list-endpoint behavior — it is your safety net.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Check | `cargo check --workspace` | exit 0 |
| Proxy tests | `cargo test -p plug-core proxy` | all pass |
| Full tests | `cargo test --workspace` | all pass (parity matrix included) |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `plug-core/src/proxy/catalog.rs` — `paginated_result` + its four callers
- `plug-core/src/proxy/mod.rs` — the `refresh_tools` classification loop and the filtered-view construction

**Out of scope** (do NOT touch):
- `PAGE_SIZE` or cursor semantics — pinned by remote-client behavior (see `docs/bug-reports/pagination-cursor-forwarding-and-remote-client-blanking.md`).
- The lazy-tools/bridge paths in `catalog.rs` (`bridge_visible_tools`, `filtered_legacy_meta_tools`).
- `server_manager` APIs — hoist lookups at the call site; do not change `get_upstream`'s signature.
- Any wire-visible output shape.

## Git workflow

- Branch: `perf/catalog-hot-path-batch`
- Conventional commits per step: `perf(proxy): paginate from borrowed slice instead of double-cloning the catalog`, `perf(proxy): hoist per-server lookups out of the refresh classification loop`, `perf(proxy): skip filtered client views when tool filtering is disabled`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Borrow in `paginated_result`

Change the signature to borrow:

```rust
pub(crate) fn paginated_result<T: Clone, R>(
    items: &[T],
    request: Option<PaginatedRequestParams>,
    build: impl FnOnce(Vec<T>, Option<String>) -> R,
) -> R {
    ...
    build(items[start..end].to_vec(), next_cursor)
}
```

Update all four callers to pass `&tools` / `&resources` / etc. instead of
`(*x).clone()` (the callers hold `Arc<Vec<T>>`; `&**arc` or `arc.as_slice()`
both work). Check for any other `paginated_result` callers first:
`grep -rn 'paginated_result' plug-core/src plug/src`.

**Verify**: `cargo test --workspace` → all pass (pagination unit tests + parity matrix). `grep -n '(\*tools).clone()\|(\*resources).clone()\|(\*prompts).clone()\|(\*templates).clone()' plug-core/src/proxy/catalog.rs` → no matches.

### Step 2: Hoist per-server data out of the refresh loop

Before the `for (server_name, tool) in upstream_tools` loop in
`refresh_tools` (`proxy/mod.rs:1112`), build a per-server context map (the
distinct servers are far fewer than the tools):

```rust
struct ServerRefreshCtx {
    renames: HashMap<String, String>,          // or clone the Arc<UpstreamServer> once
    tool_group_rules: Option<Vec<ToolGroupRule>>,
    icons: Option<...>,                        // whatever get_upstream_metadata(...).icons returns
}
let mut server_ctx: HashMap<String, ServerRefreshCtx> = HashMap::new();
```

Populate lazily on first encounter of each `server_name`
(`entry(...).or_insert_with(...)` resolving `get_upstream` +
`get_upstream_metadata` ONCE), then index into it inside the loop for
renames, rules (including the existing `server_name == "workspace"` default-
rules fallback — preserve that branch exactly), and in pass 3 for icons.
Simplest equivalent alternative: store the `Arc<UpstreamServer>` clone once
per server and borrow `config.tool_renames` / `config.tool_groups` from it —
avoids the rules Vec clone entirely. Either shape is acceptable; the
requirement is: per distinct server, at most one `get_upstream` and one
`get_upstream_metadata` call per refresh, and no per-tool `Vec` clone of
`tool_groups`.

**Verify**: `cargo test --workspace` → all pass (the naming/grouping tests in the proxy suite pin rename/group behavior; the `workspace` default-rules fallback has dedicated tests — confirm by running `cargo test -p plug-core tool_naming` and `cargo test -p plug-core proxy`).

### Step 3: Gate the filtered views

At `proxy/mod.rs:1338-1339`, build the views only when they can be read:

```rust
let (tools_windsurf, tools_copilot) = if self.config.tool_filter_enabled {
    (
        Arc::new(tools.iter().take(100).cloned().collect()),
        Arc::new(tools.iter().take(128).cloned().collect()),
    )
} else {
    (Arc::new(Vec::new()), Arc::new(Vec::new()))
};
```

Confirm first that `catalog.rs:542` (`if !self.config.tool_filter_enabled { return self.list_tools(); }`)
is the ONLY read path for `tools_windsurf` / `tools_copilot`:
`grep -rn 'tools_windsurf\|tools_copilot' plug-core/src plug/src` — every hit
must be either the construction site, the snapshot struct definition, or
behind the `tool_filter_enabled` check. If any other reader exists, STOP.

**Verify**: `cargo test --workspace` → all pass. If a test constructs a router with `tool_filter_enabled = true` and asserts Windsurf/Copilot truncation, it must still pass unchanged.

## Test plan

- Step 1: add one unit test next to the existing pagination tests in
  `catalog.rs`/`proxy/tests.rs` asserting a mid-cursor page from a borrowed
  slice returns identical items + `next_cursor` as before (use the existing
  pagination test as the pattern — find it via `grep -rn 'next_cursor' plug-core/src/proxy`).
- Step 2: no new tests required — behavior is pinned by existing
  rename/group/icon tests; the change is a pure hoist.
- Step 3: add a unit test: with `tool_filter_enabled = false`, after a
  refresh, `list_tools_for_client_session(ClientType::Windsurf, None)`
  returns the FULL catalog (not an empty view) — this pins the invariant the
  gate depends on.
- Verification: `cargo test --workspace` → all pass, 2 new tests.

## Done criteria

- [ ] `cargo test --workspace` exits 0, with the 2 new tests present
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo fmt --check` exits 0
- [ ] `paginated_result` takes `&[T]`; no `(*…).clone()` remains at its call sites
- [ ] The refresh loop performs no `get_upstream`/`get_upstream_metadata` call inside the per-tool body (`grep -n 'get_upstream' plug-core/src/proxy/mod.rs` — remaining hits are outside the loop ranges ~1110-1250)
  > **Reviewer amendment (2026-07-12, at execution):** two hits legitimately remain
  > textually inside that range — they sit inside the `ServerRefreshCtx`
  > `entry().or_insert_with()` closure, which Step 2's own instructions require and
  > which executes at most once per distinct server per refresh. The criterion's
  > intent (no per-TOOL lookup) is met; the line-range phrasing was imprecise.
- [ ] No files outside the in-scope list modified (`git status`)
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- `tools_windsurf`/`tools_copilot` have a reader NOT behind `tool_filter_enabled` (step 3 grep).
- The `workspace` default-rules fallback (`server_name == "workspace"`) interacts with the hoist in a way the excerpt doesn't show (e.g. rules depend on per-tool state).
- Any parity-matrix test fails — list behavior changed observably; revert the step and report.
- Excerpts don't match live code (drift / concurrent Codex run).

## Maintenance notes

- If per-client filtered views grow beyond Windsurf/Copilot, keep the gate: build views only for clients that can read them under the current config.
- If pagination ever becomes cursor-stable across refreshes (cursors are currently plain indices), `paginated_result`'s borrow change is unaffected, but revisit the callers.
- Reviewer focus: step 2 must not change rename/group precedence order (renames → sanitize → group rules), and the lazily-built context must handle a server that disappears mid-refresh (`get_upstream` returning `None` → same behavior as today's per-tool `None` path).
