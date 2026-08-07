#!/usr/bin/env bash
# Run Plug-owned fleet truth stages without duplicating their underlying checks.
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)
conformance_check="$repo_root/scripts/check-mcp-conformance.sh"
golden_replayer="$repo_root/scripts/fleet/golden.py"
contract_checker="$repo_root/scripts/fleet/contract.py"
load_runner="$repo_root/scripts/fleet/load.py"
fault_runner="$repo_root/scripts/fleet/fault.py"

usage() {
  cat <<'EOF'
Usage: scripts/fleet-truth.sh [all|conformance|conformance-local|golden|contract|load|fault]

Stages:
  conformance        Fast gate: MCP inventory plus selector self-test
  conformance-local  Full local MCP protocol-pair regressions (runs Cargo)
  golden             Replay normalized MCP JSON-RPC golden transcripts
  contract           Check mock MCP list responses against committed contracts
  load               Concurrent sessions against a mock upstream (default: 2 x 5m)
  fault              Expected failure/recovery checks against mock upstream faults
  all                Run fast stages; load and fault stages remain opt-in (default)
EOF
}

require_conformance_check() {
  if [ ! -f "$conformance_check" ]; then
    printf 'fleet-truth: missing conformance check: %s\n' "$conformance_check" >&2
    return 1
  fi
}

run_conformance() {
  local passed=0
  local failed=0

  printf '%-16s %s\n' 'stage' 'conformance'
  require_conformance_check || {
    printf '%s\n' 'STAGE conformance FAIL'
    return 1
  }
  if "$conformance_check" inventory; then
    passed=$((passed + 1))
  else
    failed=$((failed + 1))
  fi

  if "$conformance_check" self-test; then
    passed=$((passed + 1))
  else
    failed=$((failed + 1))
  fi

  printf '%-16s %s\n' 'checks' "2 (passed=$passed failed=$failed)"
  printf '%-16s %s\n' 'scope' 'inventory + self-test (official suites not run)'
  if [ "$failed" -eq 0 ]; then
    printf '%s\n' 'STAGE conformance PASS'
    return 0
  fi

  printf '%s\n' 'STAGE conformance FAIL'
  return 1
}

run_conformance_local() {
  printf '%-16s %s\n' 'stage' 'conformance-local'
  printf '%-16s %s\n' 'scope' 'inventory + local Cargo regressions'
  require_conformance_check || {
    printf '%s\n' 'STAGE conformance-local FAIL'
    return 1
  }
  if "$conformance_check" local; then
    printf '%s\n' 'STAGE conformance-local PASS'
    return 0
  fi

  printf '%s\n' 'STAGE conformance-local FAIL'
  return 1
}

run_golden() {
  printf '%-16s %s\n' 'stage' 'golden'
  printf '%-16s %s\n' 'scope' 'mock stdio JSON-RPC transcripts (normalized diff)'
  if [ ! -f "$golden_replayer" ]; then
    printf 'fleet-truth: missing golden replayer: %s\n' "$golden_replayer" >&2
    printf '%s\n' 'STAGE golden FAIL'
    return 1
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    printf '%s\n' 'fleet-truth: python3 is required for golden replay' >&2
    printf '%s\n' 'STAGE golden FAIL'
    return 1
  fi
  if python3 "$golden_replayer" replay; then
    printf '%s\n' 'STAGE golden PASS'
    return 0
  fi

  printf '%s\n' 'STAGE golden FAIL'
  return 1
}

run_contract() {
  printf '%-16s %s\n' 'stage' 'contract'
  printf '%-16s %s\n' 'scope' 'mock tools/resources/templates/prompts lists (normalized diff)'
  if [ ! -f "$contract_checker" ]; then
    printf 'fleet-truth: missing contract checker: %s\n' "$contract_checker" >&2
    printf '%s\n' 'STAGE contract FAIL'
    return 1
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    printf '%s\n' 'fleet-truth: python3 is required for contract checks' >&2
    printf '%s\n' 'STAGE contract FAIL'
    return 1
  fi
  if python3 "$contract_checker" check; then
    printf '%s\n' 'STAGE contract PASS'
    return 0
  fi

  printf '%s\n' 'STAGE contract FAIL'
  return 1
}

run_load() {
  printf '%-16s %s\n' 'stage' 'load'
  printf '%-16s %s\n' 'scope' 'concurrent Plug sessions + mock stdio upstream'
  if [ ! -f "$load_runner" ]; then
    printf 'fleet-truth: missing load runner: %s\n' "$load_runner" >&2
    printf '%s\n' 'STAGE load FAIL'
    return 1
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    printf '%s\n' 'fleet-truth: python3 is required for load checks' >&2
    printf '%s\n' 'STAGE load FAIL'
    return 1
  fi
  if python3 "$load_runner"; then
    printf '%s\n' 'STAGE load PASS'
    return 0
  fi

  printf '%s\n' 'STAGE load FAIL'
  return 1
}

run_fault() {
  printf '%-16s %s\n' 'stage' 'fault'
  printf '%-16s %s\n' 'scope' 'mock malformed/reset/delay/SIGTERM/auth-expiry failures + recovery'
  if [ ! -f "$fault_runner" ]; then
    printf 'fleet-truth: missing fault runner: %s\n' "$fault_runner" >&2
    printf '%s\n' 'STAGE fault FAIL'
    return 1
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    printf '%s\n' 'fleet-truth: python3 is required for fault checks' >&2
    printf '%s\n' 'STAGE fault FAIL'
    return 1
  fi
  if python3 "$fault_runner"; then
    printf '%s\n' 'STAGE fault PASS'
    return 0
  fi

  printf '%s\n' 'STAGE fault FAIL'
  return 1
}

run_all() {
  local result=0
  run_conformance || result=$?
  run_golden || result=$?
  run_contract || result=$?
  printf '%s\n' 'STAGE load SKIP (opt-in: scripts/fleet-truth.sh load)'
  printf '%s\n' 'STAGE fault SKIP (opt-in: scripts/fleet-truth.sh fault)'
  printf '%s\n' 'STAGE fleet-runtime SKIP (not implemented)'
  printf '%s\n' 'STAGE fleet-official SKIP (opt-in only)'
  return "$result"
}

mode=${1:-all}
case "$mode" in
  all) run_all ;;
  conformance) run_conformance ;;
  conformance-local) run_conformance_local ;;
  golden) run_golden ;;
  contract) run_contract ;;
  load) run_load ;;
  fault) run_fault ;;
  -h|--help|help) usage ;;
  *) usage >&2; exit 2 ;;
esac
