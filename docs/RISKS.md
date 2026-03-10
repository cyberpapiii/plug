# Risk Register

This register lists only the current remaining risks on `main`.

## High

### Runtime reconfiguration scope is still undecided

**Impact:** High  
**Likelihood:** Medium

`plug` does not yet support fully live runtime reconfiguration. The remaining risk is product-scope
ambiguity rather than a known implementation defect: the project still needs an explicit decision on
whether full live reconfiguration is required for the intended production-ready bar.

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

