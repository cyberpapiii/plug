---
date: 2026-03-20
topic: plug-trace
---

# Plug Trace Requirements

## Problem Frame

`plug` now has strong runtime and operator surfaces, but diagnosing a real incident still requires too much manual assembly. Operators often need to run and mentally combine `plug status`, `plug doctor`, `plug auth status`, live-session inventory, and daemon log inspection to understand one failure. That slows dogfooding, makes bug reports inconsistent, and turns "what happened?" into a manual investigation instead of a repeatable workflow.

The goal of `plug trace` is to turn that multi-command investigation into one reproducible local artifact that captures enough current state to debug a runtime incident or share it for follow-up.

## Requirements

- R1. `plug trace` creates a timestamped local incident bundle for the current machine and current `plug` runtime state.
- R2. The bundle must capture the current outputs of the existing diagnostic surfaces that matter most for incident response:
  - runtime status
  - doctor diagnostics
  - auth status
  - live session inventory
  - linked/downstream topology summary
  - recent daemon log tail
- R3. The bundle must include a concise human-oriented summary plus raw machine-readable captures for sections whose source commands already support structured output.
- R4. `plug trace` must be best-effort rather than all-or-nothing. If one section cannot be collected, the command still produces the bundle and marks that section as unavailable or failed.
- R5. The artifact must redact or omit secrets, bearer tokens, refresh tokens, credential contents, and similarly sensitive material by default.
- R6. The command must print the final artifact path and a short summary of what was captured versus omitted.
- R7. V1 must optimize for local operator debugging and shareable bug-report evidence, not remote upload, hosted telemetry, or continuous tracing.
- R8. When recent runtime events are available, the bundle should include a recent-event section covering meaningful daemon/runtime changes such as auth, reload, health, or restart-related events.

## Success Criteria

- A user can run one command during or immediately after an incident and get a reusable artifact instead of rerunning several diagnostics by hand.
- The resulting artifact is sufficient for another person to understand the runtime/auth/topology state without first asking for `plug status`, `plug doctor`, and `plug auth status` separately.
- Partial runtime failure does not prevent bundle creation; unavailable sections are explicit.
- Sensitive credentials are not exposed in the normal artifact path.

## Scope Boundaries

- No remote upload, Proof sharing, or hosted incident service in V1.
- No continuous background recording or long-term telemetry platform in V1.
- No attempt to replace `plug explain`; this command captures evidence rather than performing causal diagnosis.
- No requirement that V1 support every possible filter or targeted trace mode.

## Key Decisions

- **Whole-system snapshot first**: V1 should capture one system-level incident bundle rather than starting with server-scoped or client-scoped trace subcommands. This keeps the first version simpler and more reliable.
- **Structured local bundle, not a single opaque blob**: V1 should produce a timestamped local bundle with a readable summary and raw captures rather than a single pasted text dump. This makes the artifact easier to inspect and extend.
- **Best-effort collection**: Missing daemon state, failed IPC calls, or unavailable log files should be recorded as unavailable sections, not treated as a fatal error for the whole trace.
- **Security over exhaustiveness**: If a field is plausibly sensitive, V1 should prefer redaction or omission rather than perfect completeness.
- **Existing command truth reused where possible**: The current `status`, `doctor`, `auth status`, and live-inventory surfaces remain the source of truth for captured runtime state.

## Alternatives Considered

- **Single pasted text report**: Rejected because it would be harder to inspect, harder to parse, and harder to extend safely.
- **Remote incident upload/sharing in V1**: Rejected because it increases product and security scope before the local bundle shape is proven useful.
- **Entity-scoped trace first (`plug trace server <name>`)**: Rejected for V1 because the most common need is to preserve the whole runtime picture during a confusing incident.

## Dependencies / Assumptions

- Existing operator commands continue to provide trustworthy JSON or text output for their domains.
- The daemon log path remains stable and readable when available.
- Recent runtime-event capture may require a small new in-memory retention path if the current event bus is still broadcast-only.

## Outstanding Questions

### Resolve Before Planning

None.

### Deferred to Planning

- [Affects R1][Technical] What should the exact artifact layout be on disk: a dedicated trace directory, a single archive, or a directory with optional later compression?
- [Affects R2][Technical] What is the smallest high-value log tail and event window for V1?
- [Affects R3][Technical] Which sections should be captured as JSON, which as text, and what should the summary format be?
- [Affects R8][Needs research] What is the cleanest way to expose recent daemon/runtime events without turning `plug trace` into a full telemetry subsystem?
- [Affects R1][Technical] Should V1 allow optional narrowing flags such as `--server` or `--since`, or should those wait until the whole-system snapshot flow is proven useful?

## Next Steps

→ `/prompts:ce-plan` for structured implementation planning
