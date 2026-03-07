# Risk Register

This register reflects the current post-Phase-3 state rather than the earlier stabilization-era
gaps.

## High

### Stateless MCP evolution may outpace the current session boundary

**Impact:** High  
**Likelihood:** Medium

The new `SessionStore` seam and design notes prepare for stateless downstream handling, but they do
not implement it. If the ecosystem moves quickly toward stateless-by-default clients, `plug` will
need a deliberate follow-on tranche.

### Tasks / post-June-2026 MCP features remain unimplemented

**Impact:** High  
**Likelihood:** Medium

`plug` now covers the core current protocol surface well, but future-facing spec work such as Tasks
and adjacent workflow primitives remains deferred.

## Medium

### Shared-runtime truth can drift again if the docs stop moving with the code

**Impact:** Medium  
**Likelihood:** Medium

The codebase already went through one major truth pass. The risk now is regression: merged behavior
changes faster than the tracked docs are updated.

### Upstream server diversity is still a practical reliability challenge

**Impact:** Medium  
**Likelihood:** Medium

`plug` now has much better continuity and resilience, but upstream MCP servers remain uneven in
quality, latency, and shutdown behavior.

### Meta-tool strategy is still intentionally minimal

**Impact:** Medium  
**Likelihood:** Medium

The current meta-tool mode is useful and truthful, but it is not a full dynamic tool-management or
quarantine system.

## Low

### TUI dependency confusion

**Impact:** Low  
**Likelihood:** Medium

Some TUI-era crates remain in the manifests even though there is no active TUI product surface.

### Windows parity

**Impact:** Low  
**Likelihood:** Medium

The current daemon/process model is still primarily exercised on Unix-like systems.
