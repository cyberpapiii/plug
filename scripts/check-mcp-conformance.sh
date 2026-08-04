#!/usr/bin/env bash
# Validate Plug's checked-in MCP evidence or explicitly run an external suite.
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)
inventory="$repo_root/docs/testing/mcp-compatibility-inventory.tsv"
legacy_suite_version=0.1.16
modern_suite_version=0.2.0-alpha.10

usage() {
  cat <<'EOF'
Usage: scripts/check-mcp-conformance.sh MODE

Modes:
  inventory               Validate and summarize checked-in evidence (default)
  local                   Run focused local protocol-pair regressions
  official-legacy-server  Run pinned stable suite against an existing endpoint
  official-modern-server  Run pinned prerelease suite against an existing endpoint

External modes require PLUG_MCP_CONFORMANCE_URL. They do not start or configure
Plug and are deliberately excluded from default repository gates.
EOF
}

validate_inventory() {
  awk -F '\t' '
    NR == 1 {
      expected = "id\tdirection\tsurface\tstatus\tevidence\tlimitation"
      if ($0 != expected) {
        print "invalid inventory header" > "/dev/stderr"
        exit 1
      }
      next
    }
    NF != 6 {
      print "inventory row " NR " has " NF " fields; expected 6" > "/dev/stderr"
      failed = 1
    }
    $1 == "" || seen[$1]++ {
      print "inventory row " NR " has an empty or duplicate id: " $1 > "/dev/stderr"
      failed = 1
    }
    $4 !~ /^(proven-local|dormant|unavailable-external|observed-peer)$/ {
      print "inventory row " NR " has unknown status: " $4 > "/dev/stderr"
      failed = 1
    }
    END {
      if (NR < 2) {
        print "inventory has no evidence rows" > "/dev/stderr"
        failed = 1
      }
      exit failed
    }
  ' "$inventory"
}

summarize_inventory() {
  validate_inventory
  printf 'MCP compatibility inventory: %s\n' "$inventory"
  awk -F '\t' 'NR > 1 { count[$4]++ } END { for (status in count) print status, count[status] }' \
    "$inventory" | sort
}

run_local() {
  summarize_inventory
  cd "$repo_root"

  cargo test -p plug-core protocol::tests::
  cargo test -p plug-core http::server::tests::modern_
  cargo test -p plug-core \
    proxy::tests::native_modern_upstream_completes_a_real_two_round_tool_call -- --exact
  cargo test -p plug-core \
    proxy::tasks::tests::modern_downstream_forces_local_wrapper_around_legacy_native_upstream -- --exact
  cargo test -p plug-core \
    proxy::tests::legacy_task_to_modern_upstream_is_rejected_before_task_or_tool_effect -- --exact
  cargo test -p plug-core --test integration_tests \
    upstream_lifecycle_modes_negotiate_independently_with_live_truth -- --exact

  printf '%s\n' \
    'Local evidence passed. This is not an official MCP conformance result.'
}

require_external_url() {
  if [[ -z "${PLUG_MCP_CONFORMANCE_URL:-}" ]]; then
    printf '%s\n' \
      'PLUG_MCP_CONFORMANCE_URL is required and must point to an already-running disposable Plug endpoint.' \
      >&2
    exit 2
  fi
  case "$PLUG_MCP_CONFORMANCE_URL" in
    http://*|https://*) ;;
    *)
      printf 'PLUG_MCP_CONFORMANCE_URL must be an http(s) URL, got: %s\n' \
        "$PLUG_MCP_CONFORMANCE_URL" >&2
      exit 2
      ;;
  esac
}

run_external_server() {
  local suite_version=$1
  local maturity=$2
  local spec_version=$3
  require_external_url

  local results_dir=${PLUG_MCP_CONFORMANCE_RESULTS_DIR:-}
  if [[ -z "$results_dir" ]]; then
    results_dir=$(mktemp -d "${TMPDIR:-/tmp}/plug-mcp-conformance.XXXXXX")
  else
    mkdir -p "$results_dir"
    results_dir=$(cd "$results_dir" && pwd)
  fi

  printf 'Running %s MCP conformance suite %s for %s against %s\n' \
    "$maturity" "$suite_version" "$spec_version" "$PLUG_MCP_CONFORMANCE_URL"
  printf 'Results directory: %s\n' "$results_dir"
  (
    cd "$results_dir"
    npx --yes "@modelcontextprotocol/conformance@$suite_version" server \
      --url "$PLUG_MCP_CONFORMANCE_URL" --suite active \
      --spec-version "$spec_version"
  )
}

mode=${1:-inventory}
case "$mode" in
  inventory) summarize_inventory ;;
  local) run_local ;;
  official-legacy-server)
    run_external_server "$legacy_suite_version" stable 2025-11-25
    ;;
  official-modern-server)
    run_external_server "$modern_suite_version" prerelease 2026-07-28
    ;;
  -h|--help|help) usage ;;
  *) usage >&2; exit 2 ;;
esac
