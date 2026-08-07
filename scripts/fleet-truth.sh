#!/usr/bin/env bash
# Run Plug-owned fleet truth stages without duplicating their underlying checks.
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)
conformance_check="$repo_root/scripts/check-mcp-conformance.sh"

usage() {
  cat <<'EOF'
Usage: scripts/fleet-truth.sh [all|conformance|conformance-local]

Stages:
  conformance        Fast gate: MCP inventory plus selector self-test
  conformance-local  Full local MCP protocol-pair regressions (runs Cargo)
  all                Run conformance; report later suite stages as SKIP (default)
EOF
}

run_conformance() {
  local passed=0
  local failed=0

  printf '%-16s %s\n' 'stage' 'conformance'
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
  if "$conformance_check" local; then
    printf '%s\n' 'STAGE conformance-local PASS'
    return 0
  fi

  printf '%s\n' 'STAGE conformance-local FAIL'
  return 1
}

run_all() {
  local result=0
  run_conformance || result=$?
  printf '%s\n' 'STAGE fleet-runtime SKIP (not implemented)'
  printf '%s\n' 'STAGE fleet-official SKIP (opt-in only)'
  return "$result"
}

mode=${1:-all}
case "$mode" in
  all) run_all ;;
  conformance) run_conformance ;;
  conformance-local) run_conformance_local ;;
  -h|--help|help) usage ;;
  *) usage >&2; exit 2 ;;
esac
