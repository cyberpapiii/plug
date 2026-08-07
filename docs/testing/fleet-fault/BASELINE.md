# Fleet fault baseline

Captured 2026-08-07 on Linux with Rust 1.97.1.

## Scope

`scripts/fleet-truth.sh fault` injects five deterministic stdio-upstream faults
through the existing `mock-mcp-server --fail-mode` interface:

| Fault | Expected failure | Expected recovery | Result |
| --- | --- | --- | --- |
| malformed frame | Plug returns a one-second request timeout after invalid JSON-RPC | supervised upstream restart; next call succeeds | PASS |
| reset | Plug returns `Transport closed` when the upstream closes mid-call | a fresh Plug runtime connects to the restarted upstream | PASS |
| slow delay | Plug returns a one-second request timeout | supervised upstream restart; next call succeeds | PASS |
| SIGTERM | Plug returns `Transport closed` after the upstream receives SIGTERM mid-call | a fresh Plug runtime connects to the restarted upstream | PASS |
| auth expiry | Plug passes through simulated error `-32001` | the next call succeeds in the same runtime | PASS |

The auth-expiry case is an in-process protocol fixture. It does not contact a
live OAuth vendor or read or mutate stored OAuth credentials.

Reset and SIGTERM do not claim automatic same-runtime recovery. Their measured
contract is expected failure followed by recovery from a fresh Plug runtime;
the harness reports that distinction explicitly instead of changing product
behavior to make the fault disappear.

## Command

```text
$ scripts/fleet-truth.sh fault
fault summary    5 passed; 0 failed
STAGE fault PASS
```

## Default-suite choice

The `fault` stage remains opt-in under `scripts/fleet-truth.sh all`, like the
load stage. The fault gate builds binaries, launches multiple daemon and
upstream processes, deliberately sends SIGTERM, and measured about 33 seconds
on this runner. Keeping it out of the fast default preserves a short,
non-disruptive `all` gate while retaining one-command reproducibility.
