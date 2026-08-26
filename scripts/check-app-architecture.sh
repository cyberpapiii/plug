#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODEL="$ROOT_DIR/PlugApp/PlugApp/Stores/AppModel.swift"

for forbidden in \
  'DaemonServiceManager.shared' \
  'restartDaemon' \
  'launchctl' \
  'Process('
do
  if grep -Fq "$forbidden" "$MODEL"; then
    echo "error: AppModel bypasses the installation coordinator with '$forbidden'" >&2
    exit 1
  fi
done

echo "AppModel architecture check passed"
