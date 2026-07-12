# Plan 018: Downstream OAuth conformance spike (todo 057) — gap analysis against RFC 8414 / RFC 9728 and real clients

> **Executor instructions**: This is a SPIKE plan — its deliverable is a
> findings document plus a triaged follow-up list, not production code (a
> throwaway test client under the harness or a scratch dir is fine). Follow
> the steps; if anything in the "STOP conditions" section occurs, stop and
> report. When done, update the status row in `plans/README-claude-fable.md`.
>
> **Drift check (run first)**: `git diff --stat e341625..HEAD -- plug-core/src/downstream_oauth/ todos/057-*.md`
> If downstream_oauth changed materially, the gap analysis targets the LIVE
> code. Another AI agent (Codex) may be working in this repo concurrently.

## Status

- **Priority**: P2 (the only genuinely open p1 in the todo tracker: `todos/057-ready-p1-auth-oauth-hardening-program.md`, frontmatter `status: ready`)
- **Effort**: M
- **Risk**: NONE (read/spike only; any scratch server runs on loopback)
- **Depends on**: none
- **Category**: direction / security-conformance
- **Planned at**: commit `e341625`, 2026-07-11

## Why this matters

`plug serve` exposes downstream OAuth so remote HTTP clients (e.g. Claude
Desktop's remote connector — this project's real deployment) can authorize
against plug itself. Todo 057 records that this surface was built to "works
with the clients we tried" level and flags two open concerns:

1. **Standards conformance**: MCP's authorization spec requires the resource
   server to publish OAuth 2.0 Protected Resource Metadata (RFC 9728,
   `/.well-known/oauth-protected-resource`) pointing at the authorization
   server, whose metadata (RFC 8414, `/.well-known/oauth-authorization-server`)
   must be discoverable. Clients that follow the spec strictly (new clients
   appear constantly) will fail against plug if discovery is incomplete,
   inconsistent (issuer mismatch), or only partially implemented — failures
   that look like mystery auth loops to the operator.
2. **Privacy/scope hygiene**: what plug's downstream auth actually protects,
   what token audiences/scopes it mints or accepts, and whether anything
   leaks (server names, tool lists) pre-auth.

Whether gaps exist and which matter is unknown — that's why this is a spike
with a findings doc, not a fix plan. Its output feeds the next planning
round.

## Current state

- `todos/057-ready-p1-auth-oauth-hardening-program.md` — READ FIRST in full;
  it scopes the program and references
  `docs/plans/2026-03-16-auth-oauth-hardening-ux-plan.md` (read that too;
  historical planning context, not current truth).
- Implementation: `plug-core/src/downstream_oauth/` (mod.rs `:510` region has
  the credential-dir permission handling; the module implements the
  downstream token/authorization endpoints) and its wiring in
  `plug-core/src/http/server.rs`. UPSTREAM OAuth (plug as a CLIENT to
  upstream servers, `plug-core/src/oauth.rs`) is a DIFFERENT surface — only
  in scope where 057 says so.
- Existing checks: `plug doctor` has OAuth checks — inventory what it already
  validates (`grep -rn 'oauth' plug-core/src/doctor* plug/src --include='*.rs' -il` then read the doctor OAuth sections) so findings don't re-report covered ground.
- Spec sources (fetch at execution time): MCP authorization spec (rev
  2025-11-25, the protocol version this repo targets — see README), RFC 8414,
  RFC 9728. If the executor has no web access, the MCP spec may exist
  vendored in rmcp docs (`cargo doc` output or the rmcp source in
  `~/.cargo/registry`) — otherwise STOP condition.

## Deliverable

`docs/plans/2026-07-downstream-oauth-conformance-findings-claude-fable.md`:

1. **Conformance matrix** — one row per normative requirement (MUST/SHOULD)
   from: MCP authorization spec (client↔resource-server parts), RFC 9728
   (protected resource metadata), RFC 8414 (AS metadata, where plug is the
   AS or proxies one). Columns: requirement, spec ref, plug behavior
   (file:line), verdict {conforms / gap / partial / N-A with reason}.
2. **Real-client probe results** — what an actual strict client sees (step 3).
3. **Privacy findings** — pre-auth information exposure inventory (step 4).
4. **Triaged follow-up list** — each gap: severity (breaks-strict-clients /
   spec-gap-no-known-impact / hygiene), estimated effort, and whether it
   belongs in the next fix batch. This list is the input for the next
   planning round; it must be honest about "no gap found" if that's the
   result.

Plus: update `todos/057-*.md` with a dated progress note pointing at the
findings doc (the todo stays `ready` — the spike doesn't complete the
program).

## Commands you will need

| Purpose | Command |
|---------|---------|
| Run plug locally for probing | build once (`cargo build`) then run `./target/debug/plug serve` with a scratch config in the scratch dir — NEVER against the operator's real config/daemon (see STOP conditions) |
| Discovery probes | `curl -si http://127.0.0.1:<port>/.well-known/oauth-protected-resource` and `/.well-known/oauth-authorization-server` (and the MCP-spec'd variants with path suffixes) |
| Pre-auth surface | `curl -si http://127.0.0.1:<port>/mcp` with no/invalid bearer — record status codes, `WWW-Authenticate` contents, body |
| Tests still green | `cargo test --workspace` (must be untouched at the end) |

## Scope

**In scope**:
- Reading `downstream_oauth/`, http/server.rs wiring, doctor OAuth checks, todo 057 + its referenced plan doc.
- Fetching/reading the three specs.
- Running a LOCAL scratch `plug serve` (loopback, scratch config, throwaway credentials dir) and probing it.
- The findings doc + the dated note in todo 057.

**Out of scope** (do NOT touch):
- ALL production code changes — gaps get planned later, not hot-fixed.
- The operator's real config (`~/Library/Application Support/plug/config.toml`), real daemon, launchd services, keychain entries — the scratch instance uses an isolated config dir (plug supports a config-path flag; find it via `./target/debug/plug serve --help`).
- Upstream OAuth (oauth.rs) except where todo 057 explicitly ropes it in.
- Secrets: findings reference file:line and credential TYPE only — never values (hard rule).

## Git workflow

- Branch: `docs/downstream-oauth-conformance-spike`
- Commit: `docs(plans): downstream OAuth conformance findings (todo 057 spike)`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Read the program and the implementation

Todo 057 in full; the 2026-03-16 plan doc; `downstream_oauth/` end to end;
the http wiring; doctor's OAuth checks. Write the "what plug implements
today" summary (endpoints served, flows supported, storage, token model)
that opens the findings doc.

**Verify**: summary cites file:line for every endpoint/flow claim.

### Step 2: Build the conformance matrix

Extract normative requirements from the three specs (client↔RS/AS surface
only — plug-as-AS or plug-as-metadata-proxy, whichever step 1 showed);
verdict each against the code. Where behavior can't be determined by
reading, mark "probe" and resolve in step 3.

**Verify**: every MUST from RFC 9728 §2–3 and the MCP auth spec's discovery
section has a row; no verdict left blank except "probe" rows.

### Step 3: Probe a live scratch instance

Scratch config in the scratch dir (isolated credentials dir, loopback port,
one dummy upstream or none). Run the discovery + pre-auth curl probes;
resolve every "probe" row. Record exact responses (headers + bodies,
redacting any token-like values) in the findings doc's appendix. If a strict
public MCP client is cheaply available to point at it, use it; otherwise the
curl transcript IS the strict client and say so.

**Verify**: zero "probe" verdicts remain; transcript appended.

### Step 4: Privacy inventory

From code + probes: everything reachable WITHOUT valid auth (metadata
contents, error bodies, server names, tool names, version strings). Verdict
each item: fine / hygiene issue / leak.

**Verify**: the inventory explicitly covers: discovery documents, 401/403
bodies, `WWW-Authenticate` parameters, and any unauthenticated non-MCP
routes (health endpoints — check http/server.rs routes).

### Step 5: Triage and close out

Write the follow-up list (deliverable §4); add the dated note to todo 057;
confirm `git status` shows only the findings doc + the todo note; run
`cargo test --workspace` to prove the tree is untouched.

**Verify**: all done criteria below.

## Test plan

Not applicable (spike). The probe transcript is the evidence artifact.

## Done criteria

- [ ] Findings doc exists with all four sections + probe appendix
- [ ] Conformance matrix: every row has a verdict with file:line or transcript evidence
- [ ] Todo 057 has the dated progress note; its status remains `ready`
- [ ] No production code changed; `cargo test --workspace` exits 0
- [ ] No secret VALUES anywhere in the doc (search it for `Bearer `, `token=`, key-like strings before committing)
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

- No access to the MCP authorization spec / RFCs (no web, no vendored copy) — report; the matrix can't be built from memory.
- The scratch instance cannot run isolated (config-path flag missing or it insists on the real credentials dir/keychain) — report rather than touching the operator's real state. Note from project memory: sandboxed agent shells have killed long-running listeners (exit 137) before — if `plug serve` dies immediately in your environment, probe what you can statically, mark affected rows "static-only", and report the limitation.
- Step 1 reveals downstream OAuth is materially different from todo 057's description (drifted since it was written) — update the todo's description note FIRST, then continue with the live code as truth.
- A probe finds an actively exploitable issue (e.g. auth bypass on an MCP route) — stop probing, write it up immediately as the headline finding, and flag it to the operator before finishing the rest.

## Maintenance notes

- The findings doc is the input to the next fix-planning round — each
  follow-up item should become a normal implementation plan; do not let the
  spike doc itself accrete fixes.
- If plug's protocol-version target moves past 2025-11-25, the matrix must be
  re-verified against the new MCP auth spec revision — note the version
  prominently in the doc header.
