# Concepts

Shared domain vocabulary for this project — entities, named processes, and status concepts with project-specific meaning. Seeded with core domain vocabulary, then accretes as ce-compound and ce-compound-refresh process learnings; direct edits are fine. Glossary only, not a spec or catch-all.

## Routing and downstreams

### Downstream
A client that connects to Plug and consumes the aggregated MCP surface, distinct from an Upstream that Plug connects to on the client's behalf.

Downstreams may use different transports, capabilities, and visibility policies while sharing the same upstream runtime. Their identity is also the ownership boundary for targeted notifications, active calls, tasks, and resource subscriptions.

### Route
The authoritative mapping from a downstream-visible MCP item to the Upstream and original identity that own it.

Routes are published as coherent snapshots so listing and invocation see the same ownership view. When a route changes, persistent state tied to the old owner, such as a resource subscription, must be reconciled rather than inferred only from the new snapshot.

### Resource subscription
A downstream request for change notifications on a resource URI, represented in Plug as shared membership backed by one confirmed upstream subscription.

The first member creates remote state and the last member drains it. Acknowledged membership must remain tied to the Upstream that confirmed it, including across caller cancellation and route changes.

### Shared daemon runtime
The Plug service runtime that owns reusable upstream connections and serves local downstream sessions, allowing multiple clients to share one configured MCP runtime.

It normally runs in the background, but the same shared runtime can run interactively. It is distinct from the per-client standalone fallback created when a downstream cannot connect to the shared service; operator status should identify which runtime supplied each observation.

### Runtime truth
An observation obtained from the live runtime that owns the relevant state, including explicit scope or unavailability when the complete runtime cannot be queried.

Runtime truth takes precedence over configuration inference for health, sessions, auth state, and routed capabilities; failure to inspect it is reported as unknown or unavailable, not converted into a healthy default.

## Upstream lifecycle and health

### Upstream
A backend MCP server that `plug` connects to and proxies on behalf of downstream clients; the unit of configuration, health, routing, and supervision.

An upstream is reached over one of three transports (a stdio subprocess, HTTP, or legacy SSE). It is *routable* only while its health permits routing; an unroutable upstream is skipped during tool routing but may still be recovered.

### Server health
The connection-liveness state of an upstream, advanced by periodic health-check probes: healthy, degraded, failed, or auth-required. Distinct from Availability, which describes the catalog rather than the connection.

Degraded here means probes are failing but the upstream is still routable; failed means it is not routable; auth-required is sticky until re-authentication. The streak of consecutive probe failures is what drives recovery and Supervision decisions.

### Availability
The first-class freshness state of an upstream's *catalog* (its tools, resources, and prompts): healthy, degraded, or absent. Degraded availability means the last listing failed and last-known-good is carried forward; absent means the upstream is not in the routed set. Orthogonal to Server health — an upstream can be connection-healthy but catalog-degraded, or the reverse.

### Supervision
The active process that restarts an upstream which stays degraded past a threshold (or whose circuit breaker is open), rather than waiting for a disconnect or a manual restart.

A supervised restart is bounded by an exponential inter-episode backoff so a perpetually- or partially-failing upstream cannot storm; the backoff is cleared only once the upstream has *stably* recovered, never on a transient post-restart healthy sample.

### Proactive recovery
The backoff-bounded reconnect attempted when an upstream has failed or disconnected; the shared mechanism a supervised restart reuses to actually restart (stdio) or reconnect (HTTP/SSE) the upstream.

## Flagged ambiguities

- "Degraded" names two distinct, orthogonal states: a **Server health** Degraded (probes failing, upstream still routable) and an **Availability** Degraded (catalog listing failed, serving last-known-good). They must not be conflated.
