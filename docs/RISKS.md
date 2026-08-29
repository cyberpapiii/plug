# Risk Register

This register lists only the current remaining risks on `main`.

## High

### Runtime reconfiguration scope is still undecided

**Impact:** High  
**Likelihood:** Medium

`plug` does not yet support fully live runtime reconfiguration. The remaining risk is product-scope
ambiguity rather than a known implementation defect: the project still needs an explicit decision on
whether full live reconfiguration is required for the intended production-ready bar.

### The IPC socket is bound only after the whole engine starts

**Impact:** Medium
**Likelihood:** High

`cmd_daemon` claims the runtime lock, runs `Engine::start` to completion, and
only then binds the Unix socket. Every configured upstream must connect first,
and each one can involve a Keychain read or a network round trip, so on a cold
start there is a window of tens of seconds in which the daemon is alive and
healthy but indistinguishable from absent to anything that probes the socket.

The damage this used to cause is now contained rather than removed. Callers
consult the runtime lock to tell a booting daemon from a dead one, wait 90
seconds for it, and no longer force-restart what is already starting. What
remains is that no client can talk to the daemon during startup, and no client
can be told why it is waiting.

Binding the listener before `Engine::start` and answering "starting" until the
engine is ready would close it properly. That is a real change to daemon
structure, not a tuning fix, which is why it is recorded here rather than done
alongside the containment.

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

