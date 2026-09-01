# Risk Register

This register lists only the current remaining risks on `main`.

## High

### Runtime reconfiguration scope is still undecided

**Impact:** High  
**Likelihood:** Medium

`plug` does not yet support fully live runtime reconfiguration. The remaining risk is product-scope
ambiguity rather than a known implementation defect: the project still needs an explicit decision on
whether full live reconfiguration is required for the intended production-ready bar.

### Downstream HTTP still binds only after the whole engine starts

**Impact:** Low
**Likelihood:** High

`cmd_daemon` used to run `Engine::start` to completion before binding anything,
so on a cold start there was a window of tens of seconds in which the daemon was
alive and healthy but indistinguishable from absent to anything that probed the
socket. The IPC socket now binds first and `Engine::start` runs alongside it, so
local clients reach the daemon immediately and see upstreams appear through the
`list_changed` notifications the router already sends.

Downstream HTTP deliberately keeps the old ordering. A remote client cannot be
told the catalog grew — the Claude Desktop connector does not subscribe to
notifications — so for that path a bound port has to keep meaning a complete
catalog. The cost is that a remote client which connects during a cold start is
refused rather than served a partial catalog, which is the correct trade for a
client that would otherwise never learn what it missed.

Closing this properly means either a remote client that honors `list_changed` or
a readiness answer richer than connection refused, and neither is worth doing
before one is actually needed.

## Medium

### Manual refresh command remains an open product decision

**Impact:** Medium  
**Likelihood:** Medium

OAuth refresh now works automatically in the background, but `main` still carries an open decision
about whether a manual refresh IPC command is warranted. The risk is not missing core auth support;
it is leaving operator UX and recovery policy ambiguous.

### Shared-truth docs can drift from `main` if updates lag behind merges

**Impact:** Medium  
**Likelihood:** Medium

The current truth docs are much healthier than before, but the project still depends on disciplined
post-merge maintenance of `docs/PLAN.md`, `docs/PROJECT-STATE-SNAPSHOT.md`, `CLAUDE.md`, and the
`todos/` inventory. Without that discipline, the repo can drift back into stale-status reporting.

## Low

### Daemon continuity proof is still narrower than full cross-transport recovery

**Impact:** Low  
**Likelihood:** Medium

`main` proves daemon continuity for the stdio-over-IPC recovery path, but not as a broad
cross-transport continuity guarantee. This is a remaining confidence gap, not a known regression.

