#!/usr/bin/env bash
# Build and test Plug.app the one correct way.
#
# One implementation, two callers: the CI `Test (Plug.app)` job and the local
# `scripts/dev.sh` gate. This exists because the correct invocation used to live
# only in ci.yml, and a plain `xcodebuild test` run by hand fails five fixture
# tests with an unhelpful `NSCocoaErrorDomain Code=259`. The unit suite compares
# the host bundle version against the embedded daemon, so the build has to carry
# the workspace version the way a release build does. Nobody should have to know
# that to run the tests.
#
# Usage:
#   scripts/test-app.sh              signed local build, every test
#   scripts/test-app.sh --unsigned   no code signing, signed fixtures skipped
#
# --unsigned is what CI passes. `UnifiedReconciliationFixtureTests` needs a
# Developer ID host bundle and an embedded SMAppService, which an ephemeral
# runner cannot provide, so signing and those fixtures are one condition rather
# than two flags that can be set inconsistently.
#
# The build number defaults to 1 and comes from PLUG_APP_BUILD_NUMBER when set;
# CI passes the run number so its bundles are ordered.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

unsigned=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --unsigned) unsigned=true ;;
    -h | --help)
      sed -n '2,22p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
  shift
done

if ! command -v xcodegen >/dev/null 2>&1; then
  echo "xcodegen is not installed. brew install xcodegen" >&2
  exit 1
fi

version="$(cargo metadata --no-deps --format-version 1 |
  python3 -c 'import json,sys; d=json.load(sys.stdin); print(next(p["version"] for p in d["packages"] if p["name"] == "plug-mcp"))')"
build_number="${PLUG_APP_BUILD_NUMBER:-1}"

settings=(MARKETING_VERSION="$version" CURRENT_PROJECT_VERSION="$build_number")
test_settings=("${settings[@]}")
skip=()
if [[ "$unsigned" == true ]]; then
  test_settings+=(CODE_SIGNING_ALLOWED=NO)
  skip=(-skip-testing:PlugAppTests/UnifiedReconciliationFixtureTests)
fi

# The project file is generated. Regenerating first is what makes a newly added
# Swift file actually compile, and a dirty PlugApp.xcodeproj afterwards means the
# committed copy was stale, which is worth seeing rather than hiding.
xcodegen generate --spec PlugApp/project.yml --quiet

log="$(mktemp -t plug-app-test)"
trap 'rm -f "$log"' EXIT

run_xcodebuild() {
  local label="$1"
  shift
  if ! xcodebuild "$@" >"$log" 2>&1; then
    echo "$label failed:" >&2
    grep -E 'error:|failed \(|\*\* .* FAILED \*\*' "$log" | head -40 >&2 || true
    echo "full log: $log" >&2
    trap - EXIT
    exit 1
  fi
}

# Proves the complete app and login-service bundle compile together, which the
# host-architecture test build alone does not cover. Signing is off here on
# purpose: this step answers "does it compile", and a generic destination has no
# provisioning to sign against.

# `${skip[@]+...}` because macOS still ships bash 3.2, where expanding an empty
# array under `set -u` is an error rather than nothing.
run_xcodebuild "test" test \
  -project PlugApp/PlugApp.xcodeproj \
  -scheme PlugApp \
  -destination 'platform=macOS' \
  -only-testing:PlugAppTests \
  ${skip[@]+"${skip[@]}"} \
  "${test_settings[@]}"

grep -E '^\s+Executed .* tests' "$log" | tail -1
