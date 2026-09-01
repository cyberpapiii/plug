---
title: feat: Add plug trace incident bundle command
type: feat
status: active
date: 2026-03-20
origin: docs/brainstorms/2026-03-20-plug-trace-requirements.md
---

# feat: Add plug trace incident bundle command

## Overview

Add a new operator-facing command, `plug trace`, that captures a timestamped local incident bundle for the current `plug` runtime. The first version should optimize for local debugging and shareable bug-report evidence by composing the existing diagnostic surfaces into one best-effort, redacted artifact rather than introducing hosted telemetry, continuous tracing, or remote upload (see origin: `docs/brainstorms/2026-03-20-plug-trace-requirements.md`).

## Problem Statement / Motivation

Today, a real runtime incident often requires an operator to run and mentally join several commands:

- `plug status`
- `plug doctor`
- `plug auth status`
- live-session inventory and topology views
- daemon log inspection

That workflow is truthful but fragmented. It slows dogfooding, weakens reproducibility, and makes bug reports dependent on the exact commands the operator happened to run.

The feature should turn that fragmented investigation into one repeatable local capture flow:

- one command
- one artifact path
- one summary plus raw evidence
- explicit visibility into what could and could not be captured

## Proposed Solution

Introduce `plug trace` as a top-level operator command that writes a timestamped local bundle containing:

- a human-readable summary
- raw captures from existing diagnostic surfaces
- daemon log tail
- recent runtime-event history when available
- explicit metadata for unavailable sections

The command should be best-effort, redacted by default, and biased toward reusing existing command/runtime truth instead of inventing a parallel diagnostic model (see origin: `docs/brainstorms/2026-03-20-plug-trace-requirements.md`).

### Product Shape

- Whole-system snapshot first, not entity-scoped subcommands
- Structured local bundle, not a single opaque pasted report
- Best-effort section capture, not all-or-nothing failure
- Security-first output defaults
- Local-only V1; no upload/sharing workflow

## Technical Approach

### Architecture

The feature should be built as a thin orchestration layer over existing runtime and operator surfaces:

- CLI entrypoint and output behavior in `plug/src/main.rs`
- trace command implementation in `plug/src/commands/`
- existing operator data sources:
  - `plug/src/views/overview.rs` for runtime/status behavior
  - `plug/src/commands/misc.rs` for doctor behavior
  - `plug/src/commands/auth.rs` for auth-status behavior
  - `plug/src/runtime.rs` for live-session inventory and daemon reachability
  - `plug/src/daemon.rs` for daemon log location and runtime-owned services
  - `plug-core/src/ipc.rs` for status/auth/live-session request shapes
- optional recent-event capture from the engine event stream in `plug-core/src/engine.rs`

The command should avoid scraping terminal text when a stable structured path already exists. Prefer JSON-producing paths for raw bundle sections, then layer the human-readable summary on top.

### Implementation Phases

#### Phase 1: Bundle Skeleton and Existing Diagnostic Capture

Deliverables:

- add `plug trace` to the CLI
- create a timestamped bundle directory on disk
- capture existing structured outputs for:
  - status
  - doctor
  - auth status
  - live-session inventory / topology summary
- write a summary file plus a machine-readable manifest
- print the final bundle path and section summary

Success criteria:

- the command works with no daemon-side feature changes
- bundle creation succeeds even when some sections are unavailable

#### Phase 2: Log Tail and Redaction Hardening

Deliverables:

- capture recent daemon log tail using the current daemon log path
- add redaction rules so credentials and tokens are not persisted in the trace output
- add explicit unavailable/failed section metadata

Success criteria:

- the bundle remains useful when logs exist
- missing logs do not break trace generation
- obvious secrets are not emitted in normal output

#### Phase 3: Recent Runtime Event Support

Deliverables:

- add a bounded recent-event retention path for engine/runtime events if needed
- expose a query path suitable for trace capture
- include a recent-event section covering auth, reload, health, and restart-related events

Success criteria:

- recent-event capture is bounded and queryable
- trace can include recent runtime context without becoming a general telemetry system

#### Phase 4: Polish, Contracts, and Verification

Deliverables:

- stabilize artifact layout and manifest format
- tighten summary wording and JSON contracts
- add end-to-end verification for healthy, degraded, and partially unavailable runtime states

Success criteria:

- another operator can inspect a bundle without rerunning core diagnostic commands
- the feature is safe to recommend in bug-report and dogfood workflows

## Alternative Approaches Considered

### Single pasted text report

Rejected because it would be harder to parse, harder to diff, and harder to extend safely. A structured bundle better supports both human review and later automation (see origin: `docs/brainstorms/2026-03-20-plug-trace-requirements.md`).

### Remote incident upload or hosted trace service

Rejected for V1 because it expands security, privacy, and product scope before the local bundle shape is proven useful (see origin: `docs/brainstorms/2026-03-20-plug-trace-requirements.md`).

### Entity-scoped trace first

Rejected for V1 because the most common current need is preserving the entire runtime picture during confusing operator incidents rather than tracing a single server in isolation (see origin: `docs/brainstorms/2026-03-20-plug-trace-requirements.md`).

## System-Wide Impact

### Interaction Graph

`plug trace` will sit above several existing code paths rather than replacing them:

- CLI command dispatch in `plug/src/main.rs`
- diagnostic collection through existing operator/reporting surfaces
- daemon reachability and live-session collection in `plug/src/runtime.rs`
- daemon log-path usage in `plug/src/daemon.rs`
- optional recent-event capture from the engine event bus in `plug-core/src/engine.rs`

The expected chain is:

`plug trace` triggers diagnostic collectors → collectors query current runtime/auth/live-session sources → collectors serialize structured outputs into the bundle → summary renderer records included/omitted sections.

If Phase 3 lands, the flow extends to:

`Engine` emits runtime events → bounded event retention stores recent events → `plug trace` reads the retained window into the artifact.

### Error & Failure Propagation

This command must not inherit the strictest failure mode of its sources. The trace collector should treat most section-level failures as recoverable:

- daemon unreachable
- IPC request failure
- auth-status fallback path
- missing or unreadable daemon log file
- recent-event buffer unavailable

Only top-level artifact-creation failures should abort the whole command, such as:

- bundle directory cannot be created
- manifest/summary cannot be written

Section failures should be preserved as explicit trace metadata rather than swallowed or escalated into total failure.

### State Lifecycle Risks

The feature is primarily read-only, but it does persist new local data:

- timestamped bundle directory
- summary and manifest
- raw diagnostic captures

Risks:

- trace directories accumulating without retention guidance
- partially written bundles after mid-run failure
- sensitive data accidentally persisted in raw captures

Mitigations:

- write a manifest with section status so partial bundles remain intelligible
- stage sections deterministically so incomplete output is obvious
- centralize redaction rules before writing files

### API Surface Parity

The trace command should align with existing operator surfaces instead of inventing new truth sources:

- reuse stable JSON where available from `status`, `doctor`, and `auth status`
- reuse live-session/runtime inventory semantics already exposed via IPC/runtime helpers
- avoid parallel "trace-only" notions of auth or health

If new daemon/event query surfaces are introduced, they should remain additive and useful beyond `plug trace`, not purely one-off endpoints.

### Integration Test Scenarios

The plan should cover at least these cross-layer scenarios:

1. Healthy daemon/runtime: full bundle with all core sections present
2. Daemon reachable but one runtime query unavailable: bundle succeeds with explicit section failure
3. Daemon unavailable/config-only state: bundle still includes local/config-derived sections and marks runtime sections unavailable
4. Missing daemon log file: bundle succeeds without logs and says so
5. Sensitive data path: auth-related sections and logs do not persist tokens or credential material

## Acceptance Criteria

### Functional Requirements

- [ ] `plug trace` exists as a top-level operator command
- [ ] The command creates a timestamped local incident bundle directory
- [ ] The bundle includes current captures for status, doctor, auth status, and runtime/session context
- [ ] The bundle includes a human-readable summary plus raw captures
- [ ] Section-level failures are represented explicitly without aborting the entire command
- [ ] The command prints the final bundle path and a concise included/omitted summary
- [ ] When recent runtime events are available, they are included in the bundle

### Non-Functional Requirements

- [ ] Sensitive credentials and tokens are redacted or omitted by default
- [ ] The feature remains local-only in V1
- [ ] Trace capture should complete quickly enough to be practical during a live incident

### Quality Gates

- [ ] Focused tests cover bundle creation in healthy and degraded runtime states
- [ ] Tests cover secret-redaction behavior
- [ ] Tests cover missing-log and missing-runtime-source behavior
- [ ] Docs/help output clearly describe the purpose and boundaries of `plug trace`

## Success Metrics

- Operators can attach one trace bundle to a bug report instead of manually pasting several command outputs
- A second person can understand the incident state from the bundle without first asking for `plug status`, `plug doctor`, and `plug auth status`
- Dogfood debugging requires fewer repeated command runs and less ad hoc log hunting

## Dependencies & Prerequisites

- Existing structured outputs for status, doctor, and auth status remain stable enough to reuse
- A bundle path convention must be chosen
- If recent-event capture is included in V1, a bounded retention strategy must be added to the runtime

## Open Implementation Questions

- What exact on-disk artifact layout should V1 use: trace directory only, archive only, or directory with optional later compression?
- What is the smallest useful log tail and recent-event window for V1?
- Which sections should be emitted as JSON, which as plain text, and what should the summary format look like?
- Should optional narrowing flags such as `--server` or `--since` ship in V1, or wait until the whole-system snapshot flow is proven useful?

## Risk Analysis & Mitigation

- **Secret leakage risk**: Highest risk. Mitigate with centralized redaction, explicit tests, and a conservative default policy.
- **Feature sprawl risk**: `plug trace` could expand into telemetry, upload, filtering, or diagnosis. Mitigate by keeping V1 focused on local incident bundles only.
- **Parallel truth risk**: A trace-only collector could diverge from existing operator commands. Mitigate by reusing existing structured outputs and runtime helpers.
- **Partial bundle confusion**: Operators may not trust incomplete traces. Mitigate with explicit manifest metadata and summary wording.

## Future Considerations

- optional narrowing flags such as `--server` or `--since`
- optional archive/compression step once the directory format is proven useful
- optional handoff into future `plug explain` or bug-report workflows
- optional recent-event retention tuning once real usage shows the right window size

## Documentation Plan

- update CLI help text and operator-facing docs to introduce `plug trace`
- add one short usage example to the README or operator docs
- document security and scope boundaries clearly: local-only, best-effort, redacted by default

## Sources & References

### Origin

- **Origin document:** `docs/brainstorms/2026-03-20-plug-trace-requirements.md`
  - Key decisions carried forward:
    - whole-system snapshot first
    - structured local bundle instead of a single opaque report
    - best-effort section capture
    - local-only V1 with security-first defaults

### Internal References

- `plug/src/main.rs:91` — top-level CLI command structure and display order
- `plug/src/views/overview.rs:330` — runtime/status command behavior and JSON output shape
- `plug/src/commands/misc.rs:149` — doctor command behavior and runtime interpretation
- `plug/src/commands/auth.rs:518` — auth-status command behavior and JSON envelope
- `plug/src/runtime.rs:399` — live runtime/session inventory path used by status surfaces
- `plug/src/daemon.rs:253` — daemon log directory path
- `plug-core/src/ipc.rs:18` — daemon IPC request/response shapes for status/auth/live sessions
- `plug-core/src/engine.rs:51` — engine event model and broadcast channel

### Institutional Learnings

- `docs/solutions/integration-issues/2026-03-18-doctor-setup-guidance.md`
  - diagnostic output should point to real, supported operator actions
- `docs/solutions/integration-issues/2026-03-18-control-notification-lag-signals.md`
  - explicit visibility is often a safer first step than trying to guarantee perfect delivery or completeness
- `docs/solutions/integration-issues/2026-03-18-runtime-truth-config-env-session-oauth-hardening.md`
  - read-only operator surfaces should prefer truthful reporting over convenience or implicit healing
- `docs/solutions/integration-issues/2026-03-18-auth-status-backing-store-warnings.md`
  - warnings are valuable operator signals even when they are not hard failures
- `docs/solutions/integration-issues/2026-03-18-oauth-credential-snapshot-unification.md`
  - centralize snapshot logic instead of scattering repeated backing-store reads across operator surfaces

### External References

- None. Strong local context exists, and this feature is primarily about composing repo-local operator/runtime behavior.
