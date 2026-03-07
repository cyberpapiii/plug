# Risk Register

This risk register reflects the current post-stabilization direction, not the original research-only state.

## High

### Notification infrastructure is still missing

**Impact:** High  
**Likelihood:** High

`plug` still does not forward the full server-initiated MCP notification surface. This is the next major architectural tranche after `v0.1`.

### Capability surface is still incomplete

**Impact:** High  
**Likelihood:** Medium

The current product is strongest on tools. Resources/prompts/notifications are not yet at parity with the tool path.

### June 2026 MCP changes may outpace the current abstractions

**Impact:** High  
**Likelihood:** Medium

Stateless-first MCP and Tasks will require careful boundary work. The correct near-term response is preparation, not premature implementation.

## Medium

### Shared-runtime truth can drift again if docs are not maintained

**Impact:** Medium  
**Likelihood:** Medium

The repo recently had major code/doc drift. Keeping docs accurate is now part of the release gate.

### Tool-scaling strategy is still basic

**Impact:** Medium  
**Likelihood:** Medium

Client-aware filtering exists, but the larger dynamic-discovery/meta-tool strategy is still future work.

### Upstream server diversity

**Impact:** Medium  
**Likelihood:** Medium

Some MCP servers are slow, stateful, or operationally noisy. The current runtime mitigates part of this, but not all of it.

## Low

### TUI dependency confusion

**Impact:** Low  
**Likelihood:** Medium

The manifests still include TUI-era crates. This is mostly a maintenance/documentation issue until a future cleanup removes or justifies them.

### Windows parity

**Impact:** Low  
**Likelihood:** Medium

The current development focus is not Windows-specific parity in daemon/process semantics.
