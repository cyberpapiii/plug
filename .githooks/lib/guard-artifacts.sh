#!/usr/bin/env bash
# Keep generated artifacts inside budget without anyone having to remember.
# The guard is silent below budget and costs about 40 milliseconds, so it can
# run on ordinary git activity rather than waiting for a cleanup ritual.
#
# post-commit, post-merge, and post-checkout all want exactly this, so they are
# thin wrappers around one body. Git ignores files here that are not named after
# a hook, so this script never runs on its own.
set -euo pipefail
[[ "${PLUG_SKIP_HOOKS:-0}" == "1" ]] && exit 0
repo_root="$(git rev-parse --show-toplevel)"
"$repo_root/scripts/clean-build-artifacts.sh" --guard || true
