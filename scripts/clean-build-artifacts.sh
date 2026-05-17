#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/clean-build-artifacts.sh [--yes] [--runtime-cache]

Cleans generated Plug build/deploy artifacts.

Default mode is a dry run. Pass --yes to remove files.

Removed with --yes:
  - this repo's Cargo target directory via `cargo clean`
  - temporary Plug audit/install directories under /tmp

Removed only with --yes --runtime-cache:
  - ~/Library/Caches/plug/artifacts

Never removed:
  - ~/Library/Application Support/plug
  - OAuth tokens, config files, sockets, PID files
  - ~/.cargo/bin/plug or ~/.local/bin/plug
USAGE
}

confirm=false
runtime_cache=false

for arg in "$@"; do
  case "$arg" in
    --yes)
      confirm=true
      ;;
    --runtime-cache)
      runtime_cache=true
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $arg" >&2
      usage >&2
      exit 2
      ;;
  esac
done

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

size_of() {
  if [[ -e "$1" ]]; then
    du -sh "$1" 2>/dev/null | awk '{print $1}'
  else
    printf '0B'
  fi
}

tmp_dirs=()
while IFS= read -r dir; do
  tmp_dirs+=("$dir")
done < <(find /tmp -maxdepth 1 -type d -name 'plug-*' -print 2>/dev/null | sort)

target_dir="$repo_root/target"
artifact_cache="${HOME}/Library/Caches/plug/artifacts"

echo "Plug generated artifact cleanup"
echo
echo "Repo target:     $target_dir ($(size_of "$target_dir"))"
echo "Runtime cache:   $artifact_cache ($(size_of "$artifact_cache"))"
echo "Temporary dirs:  ${#tmp_dirs[@]}"
if ((${#tmp_dirs[@]} > 0)); then
  for dir in "${tmp_dirs[@]}"; do
    echo "  - $dir ($(size_of "$dir"))"
  done
fi
echo

if [[ "$confirm" != true ]]; then
  echo "Dry run only. Re-run with --yes to clean build/deploy artifacts."
  echo "Add --runtime-cache to also remove old plug://artifact cache entries."
  exit 0
fi

(
  cd "$repo_root"
  cargo clean
)

if ((${#tmp_dirs[@]} > 0)); then
  for dir in "${tmp_dirs[@]}"; do
    rm -rf "$dir"
  done
fi

if [[ "$runtime_cache" == true ]]; then
  rm -rf "$artifact_cache"
fi

echo
echo "Cleanup complete."
