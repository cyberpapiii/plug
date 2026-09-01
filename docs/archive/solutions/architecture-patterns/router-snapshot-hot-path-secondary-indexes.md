---
title: "Build RouterSnapshot secondary indexes at catalog publish for O(1) tools/call resolution"
date: 2026-08-07
category: architecture-patterns
module: plug-core/proxy
problem_type: architecture_pattern
component: tooling
severity: medium
summary: "RouterSnapshot already pre-cached client-filtered tool lists for O(1) list; extend the same publish-time pattern with routes_lower, tools_by_name, and tools_by_name_lower via with_indexes(), and resolve through resolve_route()/tool_by_name() so Downstream tools/call stays O(1) after Catalog refresh."
applies_when:
  - "Catalog refresh or route publish replaces the ArcSwap RouterSnapshot used by downstream tools/call"
  - "tools/call or name lookup must resolve a Route to an Upstream without scanning the full routing table"
  - "Case-insensitive tool name fallback must not repeatedly lowercase map keys on the hot path"
  - "Client-filtered tool lists are already pre-cached on the snapshot and call routing should match that O(1) posture"
tags:
  - router-snapshot
  - secondary-indexes
  - hot-path
  - o1-lookup
  - tools-call
  - catalog-refresh
  - mcp
  - rust
related:
  - docs/solutions/integration-issues/phase3-resilience-token-efficiency.md
  - docs/solutions/integration-issues/phase3a-meta-tool-mode-tool-drift-20260307.md
  - docs/solutions/integration-issues/rmcp-sdk-integration-patterns-plug-20260303.md
  - plug-core/src/proxy/mod.rs
  - plug-core/src/proxy/tests.rs
---

# Hot-path RouterSnapshot secondary indexes for O(1) tool resolution

## Context

`RouterSnapshot` already pre-computes client-filtered tool lists (`tools_all`, `tools_windsurf`, `tools_copilot`) at refresh time so `list_tools_for_client` is an `Arc::clone` rather than a per-request filter. That pattern is documented in [Phase 3 Resilience & Token Efficiency](../integration-issues/phase3-resilience-token-efficiency.md) under “RouterSnapshot with Pre-Cached Filtered Views.”

A complexity-optimizer pass found the complementary gap: **tools/call routing and tool-name lookup still paid linear work on every request**. Hot paths walked or rebuilt lowercase maps over `routes` / `tools_all` instead of consulting indexes that could be baked into the same immutable snapshot. Case-insensitive fallback made the cost worse because each miss repeated ASCII lowercasing across the full map.

This learning extends the pre-cached-views rule: anything the hot path needs for O(1) resolution belongs in the snapshot built at finalize/publish time, not recomputed on every call. As of this writing the change lives in the local working tree and is not yet merged to `main`.

## Guidance

Keep `RouterSnapshot` as the single atomically swapped catalog (`ArcSwap`), and make secondary lookup indexes part of that immutable value.

1. **Store exact and ASCII-lowercased secondary maps on the snapshot.** Alongside `routes` and `tools_all`, the snapshot carries `routes_lower`, `tools_by_name`, and `tools_by_name_lower` (`plug-core/src/proxy/mod.rs:71-76`). Exact maps preserve canonical casing; lower maps give O(1) case-insensitive fallback without scanning.

2. **Build indexes once with `with_indexes()` after routes/tools are finalized.** `RouterSnapshot::with_indexes` derives `routes_lower` from `routes`, and both tool-name maps from enumerated `tools_all` indices (`plug-core/src/proxy/mod.rs:94-114`). Refresh finalize constructs empty index maps, then chains `.with_indexes()` before publish (`plug-core/src/proxy/mod.rs:2228-2247`).

3. **Resolve through helpers that try exact then lower.** `resolve_route` and `tool_by_name` are the lookup surface hot paths should use (`plug-core/src/proxy/mod.rs:116-127`). Exact key first; on miss, one `to_ascii_lowercase` and a second HashMap get. `tool_by_name` returns `&Tool` via the stored index into `tools_all`, avoiding a second name scan.

4. **Publish defensively if indexes were omitted.** `publish_route_snapshot` detects empty secondary maps while primary data is non-empty (`needs_indexes`, `plug-core/src/proxy/mod.rs:1001-1008`) and rebuilds via `clone().with_indexes()` before swapping. That keeps ad-hoc or test publishers from shipping a half-indexed snapshot into the live cache.

5. **Use the helpers on every tools/call and name-lookup hot path.** Enqueue/authorization, route-identity capture, direct call routing, and meta-tool search result materialization should go through `resolve_route` / `tool_by_name` after `cache.load()` / `load_full()`.

6. **Construct test snapshots with `.with_indexes()`.** Unit fixtures that hand-build `RouterSnapshot` should chain `.with_indexes()`, or publish via `replace_snapshot` / `publish_route_snapshot` (which call `with_indexes()` internally), so tests exercise the same O(1) paths production refresh publishes.

## Why This Matters

List filtering and call routing share one refresh cadence: catalogs change infrequently relative to tools/call volume. Pre-caching filtered views fixed the list side; secondary indexes fix the call side. Without them, each request either scanned `tools_all` / `routes` or rebuilt lowercase maps on the fly, multiplying cost with tool count and with case-insensitive fallback.

Because indexes live inside the immutable snapshot, readers stay lock-free under `ArcSwap`: a load yields routes, filtered lists, and lookup maps that are consistent with each other for that generation. The lazy `needs_indexes` path is a safety net, not the steady state—refresh and tests should still call `with_indexes()` so publish does not clone on every swap.

## When to Apply

- Hot paths need name → route or name → tool resolution more often than catalogs refresh.
- Lookups need both exact-match and case-insensitive fallback.
- The catalog is already published as an atomically swapped immutable snapshot (or equivalent).
- Linear scans over `tools_all` / route maps show up in complexity or profiling passes.
- You are extending the RouterSnapshot pre-cached filtered-views pattern rather than inventing a second mutable index store.

Do not maintain a separate mutable DashMap of lowercase keys beside the snapshot unless entries mutate independently; that splits truth across two owners. Prefer rebuild-on-refresh indexes co-published with the catalog.

## Examples

**Index construction at finalize time** (`plug-core/src/proxy/mod.rs:94-114`):

```rust
pub(crate) fn with_indexes(mut self) -> Self {
    self.routes_lower = self
        .routes
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
        .collect();
    self.tools_by_name = self
        .tools_all
        .iter()
        .enumerate()
        .map(|(i, tool)| (tool.name.to_string(), i))
        .collect();
    self.tools_by_name_lower = self
        .tools_all
        .iter()
        .enumerate()
        .map(|(i, tool)| (tool.name.to_ascii_lowercase(), i))
        .collect();
    self
}
```

**O(1) resolve with case-insensitive fallback** (`plug-core/src/proxy/mod.rs:116-127`):

```rust
pub(crate) fn resolve_route(&self, tool_name: &str) -> Option<&(String, String)> {
    self.routes
        .get(tool_name)
        .or_else(|| self.routes_lower.get(&tool_name.to_ascii_lowercase()))
}

pub(crate) fn tool_by_name(&self, name: &str) -> Option<&Tool> {
    self.tools_by_name
        .get(name)
        .or_else(|| self.tools_by_name_lower.get(&name.to_ascii_lowercase()))
        .and_then(|&i| self.tools_all.get(i))
}
```

**Publish-time safety net** (`plug-core/src/proxy/mod.rs:1001-1008`):

```rust
fn publish_route_snapshot(&self, snapshot: Arc<RouterSnapshot>) {
    let needs_indexes = (snapshot.routes_lower.is_empty() && !snapshot.routes.is_empty())
        || (snapshot.tools_by_name.is_empty() && !snapshot.tools_all.is_empty());
    let snapshot = if needs_indexes {
        Arc::new((*snapshot).clone().with_indexes())
    } else {
        snapshot
    };
    // ... material route publish / ArcSwap store
}
```

**Refresh builds empty maps then indexes before publish** (`plug-core/src/proxy/mod.rs:2228-2252`):

```rust
let snapshot = Arc::new(
    RouterSnapshot {
        routes,
        routes_lower: HashMap::new(),
        tools_by_name: HashMap::new(),
        tools_by_name_lower: HashMap::new(),
        tools_all,
        // ... other catalog fields
    }
    .with_indexes(),
);
self.publish_route_snapshot(snapshot);
```

| Concern | Before | After |
| --- | --- | --- |
| Client list filtering | Pre-cached `tools_*` views | Unchanged |
| tools/call route resolve | Scan / rebuild lowercase map | `resolve_route` HashMap get |
| Tool struct by name | Scan `tools_all` | `tools_by_name` → index → `tools_all[i]` |
| Consistency | Snapshot swap for lists only | Indexes co-published with routes/tools |
