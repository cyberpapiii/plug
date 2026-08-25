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
  self-test               Check selector failure detection without running Cargo
  official-legacy-server  Run pinned stable suite against an existing endpoint
  official-modern-server  Run pinned prerelease suite against an existing endpoint

External modes require PLUG_MCP_CONFORMANCE_URL. They do not start or configure
Plug and are deliberately excluded from default repository gates.
EOF
}

local_specs=(
  $'core\tprotocol::tests::'
  $'core\thttp::server::tests::modern_'
  $'core\tproxy::tests::native_modern_upstream_completes_a_real_two_round_tool_call'
  $'core\tproxy::tasks::tests::modern_downstream_forces_local_wrapper_around_legacy_native_upstream'
  $'core\tproxy::tests::legacy_task_to_modern_upstream_is_rejected_before_task_or_tool_effect'
  $'integration\tupstream_lifecycle_modes_negotiate_independently_with_live_truth'
)

selector_matches_list() {
  local selector=$1
  awk -v selector="$selector" '
    index($0, selector) > 0 && $0 ~ /: test$/ { found = 1 }
    END { exit(found ? 0 : 1) }
  '
}

assert_selector_present() {
  local selector=$1
  local listed_tests=$2
  if ! selector_matches_list "$selector" <<<"$listed_tests"; then
    printf 'local MCP selector matched zero tests: %s\n' "$selector" >&2
    return 1
  fi
}

validate_local_evidence_mapping() {
  local evidence selector spec_selector mapped
  while IFS= read -r evidence; do
    IFS=';' read -ra selectors <<<"$evidence"
    for selector in "${selectors[@]}"; do
      selector=${selector#"${selector%%[![:space:]]*}"}
      selector=${selector%"${selector##*[![:space:]]}"}
      mapped=false
      for spec in "${local_specs[@]}"; do
        IFS=$'\t' read -r _ spec_selector <<<"$spec"
        if [[ "$selector" == "$spec_selector" ]]; then
          mapped=true
          break
        fi
      done
      if [[ "$mapped" != true ]]; then
        printf 'proven-local evidence has no local selector mapping: %s\n' "$selector" >&2
        return 1
      fi
    done
  done < <(awk -F '\t' 'NR > 1 && $4 == "proven-local" { print $5 }' "$inventory")
}

preflight_local_selectors() {
  local core_tests integration_tests spec target selector
  core_tests=$(cargo test -p plug-core --lib -- --list)
  integration_tests=$(cargo test -p plug-core --test integration_tests -- --list)
  for spec in "${local_specs[@]}"; do
    IFS=$'\t' read -r target selector <<<"$spec"
    case "$target" in
      core) assert_selector_present "$selector" "$core_tests" ;;
      integration) assert_selector_present "$selector" "$integration_tests" ;;
      *) printf 'unknown local selector target: %s\n' "$target" >&2; return 1 ;;
    esac
  done
}

run_self_test() {
  local fixture=$'protocol::tests::legacy_round_trip: test\nhttp::server::tests::modern_discovery: test'
  assert_selector_present 'protocol::tests::' "$fixture"
  if assert_selector_present 'definitely::bogus::selector' "$fixture" 2>/dev/null; then
    printf '%s\n' 'selector self-test failed: bogus selector unexpectedly matched' >&2
    return 1
  fi
  printf '%s\n' 'Selector zero-match self-test passed.'
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

  validate_local_evidence_mapping
  preflight_local_selectors

  cargo test -p plug-core --lib protocol::tests::
  cargo test -p plug-core --lib http::server::tests::modern_
  cargo test -p plug-core --lib \
    proxy::tests::native_modern_upstream_completes_a_real_two_round_tool_call -- --exact
  cargo test -p plug-core --lib \
    proxy::tasks::tests::modern_downstream_forces_local_wrapper_around_legacy_native_upstream -- --exact
  cargo test -p plug-core --lib \
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
  self-test) run_self_test ;;
  official-legacy-server)
    run_external_server "$legacy_suite_version" stable 2025-11-25
    ;;
  official-modern-server)
    run_external_server "$modern_suite_version" prerelease 2026-07-28
    ;;
  -h|--help|help) usage ;;
  *) usage >&2; exit 2 ;;
esac
