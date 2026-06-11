# Concepts

Shared domain vocabulary for this project — entities, named processes, and status concepts with project-specific meaning. Seeded with core domain vocabulary, then accretes as ce-compound and ce-compound-refresh process learnings; direct edits are fine. Glossary only, not a spec or catch-all.

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
