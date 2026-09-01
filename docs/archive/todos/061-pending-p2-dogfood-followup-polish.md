---
status: pending
priority: p2
issue_id: "061"
tags: [ux, dogfood, polish, operator, auth, runtime]
dependencies: ["060"]
---

# Dogfood Follow-Up Polish

## Problem Statement

The broad operator/auth/runtime cleanup is on `main`, but the next meaningful improvements should
come from real usage rather than another speculative sweep. There will still be small rough edges,
copy issues, confusing states, or recovery gaps that only show up while using `plug` day to day.

## Goal

Keep one lightweight place to capture and execute real dogfood findings without reopening a large
architecture phase.

## Scope

Use this tracker for issues like:

- confusing wording in `status`, `clients`, `auth status`, or `doctor`
- unclear live vs linked vs fallback behavior
- recovery flows that still require too much guesswork
- auth/runtime edge cases that surface awkwardly in normal use
- small UI/data mismatches that don’t warrant a new broad program

## Task List

This is a standing lane, not a finite checklist — the boxes stay open while dogfooding continues.

- [ ] Task 1: capture real usage findings as they occur (1 logged: 2026-08-08)
- [ ] Task 2: group findings into copy, UX, auth/recovery, or runtime buckets (1 bucketed: auth/recovery)
- [ ] Task 3: execute small fixes in narrow, well-verified slices (1 fixed: finding 1)
- [ ] Verification: each landed fix has focused tests or live smoke evidence (finding 1: regression test + live repro)

## Intake Notes

Start with issues observed directly while using the current `main` build. Prefer concrete repros and
exact command output over general impressions.

## Work Log

### 2026-03-17 - Tracker created

**By:** Codex

**Actions:**
- Created a dedicated post-polish tracker so remaining work can be driven by real dogfooding.
- Explicitly scoped this as a narrow follow-up lane, not a new architecture program.

**Learnings:**
- The system is now clean enough that the highest-value remaining work is best discovered through
  actual daily use.

### 2026-08-08 - Finding 1: disabled OAuth servers reported as degraded (auth/recovery bucket)

**By:** Claude Fable 5

**Repro (live `main` build, plug 0.3.0, daemon PID 49739):**

`plug doctor` reported `! runtime_auth_degraded  degraded auth/runtime: supabase` and escalated
`doctor_interpretation` to a warning. `plug auth status` reported
`! supabase (authenticated, degraded)` with `Token expires in: 0s` and the hint
"compare `plug status` and `plug doctor`". But `plug status` lists 12 servers and does **not**
include `supabase` — because `[servers.supabase]` has `enabled = false`. The suggested recovery
step therefore led to a server that is not in the output being compared against.

The same `plug doctor` run was internally inconsistent: its `oauth_tokens` check listed only
krisp/notion/todoist (it already filters on `enabled`), while `runtime_auth_degraded` listed
supabase.

**Root cause:** `plug/src/daemon/auth_status.rs` selected OAuth servers with
`filter(|(_, sc)| sc.auth.as_deref() == Some("oauth"))`, with no `&& sc.enabled`. A disabled
server has no runtime status entry, so the health fallback in the same function classified it
`Degraded` whenever stored credentials existed — permanently, since a disabled server is never
refreshed. Every sibling OAuth surface already filtered on `enabled`
(`plug-core/src/doctor.rs:1029`, `:1099`; `plug-core/src/engine.rs:481`, `:829`, `:906`), so this
was an isolated omission rather than a deliberate difference.

**Fix:** added `&& sc.enabled` to match the siblings, plus regression test
`auth_status_omits_disabled_servers_with_stored_credentials`.

**Learnings:**
- The health fallback "credentials exist but no runtime status ⇒ Degraded" is only correct for
  servers that are *supposed* to have a runtime presence. Any surface using that fallback has to
  filter disabled servers first or it manufactures a permanent false warning.
- A recovery hint that says "compare with `plug status`" is only safe when the two commands share
  a server-selection rule.
