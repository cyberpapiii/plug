#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/clean-build-artifacts.sh [mode...]

Cleans generated Plug build/deploy artifacts. Default mode is a dry run.

Modes:
  (none)            report sizes only, remove nothing
  --guard           remove regenerable caches only if the repo is over budget;
                    silent when it is not. Also drops this project's Xcode
                    DerivedData past its own budget. Safe to run after every
                    build; git hooks and scripts/dev.sh call it for you.
  --litter          remove loose local droppings: *.profraw, .DS_Store,
                    /tmp/plug-* audit dirs, and the empty script/ stub
  --incremental     remove target/*/incremental (rebuilt on next build)
  --xcode           remove this project's Xcode DerivedData
  --yes             full `cargo clean` plus --litter
  --runtime-cache   also remove ~/Library/Caches/plug/artifacts

Modes combine. `--yes --xcode --runtime-cache` clears everything generated.

Cost note: a cold `cargo build --workspace --all-targets` on this machine is
roughly half a minute, so --yes is cheap. Dropping incremental costs less.

The --guard budgets default to 10 GB for target/ and 5 GB for DerivedData.
Override with PLUG_TARGET_BUDGET_GB and PLUG_DERIVED_BUDGET_GB.

Never removed:
  - ~/Library/Application Support/plug
  - OAuth tokens, config files, sockets, PID files
  - ~/.cargo/bin/plug or ~/.local/bin/plug
  - ~/.cargo/registry (shared across every Rust project on this Mac)
USAGE
}

# Budget for --guard, derived from measurement rather than guessed. On this
# workspace a clean `build --all-targets` lands at 3.3 GB, clippy takes it to
# 3.9 GB, and a full `cargo test --workspace` finishes at 4.2 GB. 10 GB is
# roughly 2.4x that steady state, so the guard stays silent through normal work
# and only speaks when something has genuinely accumulated.
GUARD_BUDGET_GB="${PLUG_TARGET_BUDGET_GB:-10}"

# The guard can only reclaim regenerable caches. If dropping them still leaves
# the tree far over budget, that is a full-clean situation worth mentioning.
# Escalating at the budget itself would nag on every build once the working set
# drifted up by a few hundred megabytes, and a check people learn to ignore is
# worse than no check.
GUARD_ESCALATE_FACTOR=1.5

# Xcode DerivedData for this project grows on its own schedule and Xcode
# rebuilds it without being asked, so the guard treats it as a separate,
# fully disposable budget rather than folding it into the cargo one.
GUARD_DERIVED_BUDGET_GB="${PLUG_DERIVED_BUDGET_GB:-5}"

confirm=false
runtime_cache=false
litter=false
incremental=false
xcode=false
guard=false

for arg in "$@"; do
  case "$arg" in
    --yes)
      confirm=true
      litter=true
      ;;
    --runtime-cache) runtime_cache=true ;;
    --litter) litter=true ;;
    --incremental) incremental=true ;;
    --xcode) xcode=true ;;
    --guard) guard=true ;;
    -h | --help)
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
target_dir="$repo_root/target"
artifact_cache="${HOME}/Library/Caches/plug/artifacts"
derived_data="${HOME}/Library/Developer/Xcode/DerivedData"

size_of() {
  if [[ -e "$1" ]]; then
    du -sh "$1" 2>/dev/null | awk '{print $1}'
  else
    printf '0B'
  fi
}

gb_of() {
  if [[ -e "$1" ]]; then
    du -sk "$1" 2>/dev/null | awk '{printf "%.1f", $1 / 1048576}'
  else
    printf '0.0'
  fi
}

plugapp_derived_data() {
  [[ -d "$derived_data" ]] || return 0
  find "$derived_data" -maxdepth 1 -type d -name 'PlugApp-*' -print 2>/dev/null | sort
}

tmp_dirs=()
while IFS= read -r dir; do
  tmp_dirs+=("$dir")
done < <(find /tmp -maxdepth 1 -type d -name 'plug-*' -print 2>/dev/null | sort)

remove_litter() {
  local removed=0
  while IFS= read -r file; do
    rm -f "$file"
    removed=$((removed + 1))
    # `target` and `.git` are pruned rather than filtered afterwards. --guard
    # runs after every build, and walking a multi-gigabyte target tree to throw
    # the results away would make the guard cost more than what it reclaims.
  done < <(find "$repo_root" \( -path "$repo_root/target" -o -path "$repo_root/.git" \) -prune -o \
    \( -name '*.profraw' -o -name '*.profdata' -o -name '.DS_Store' \) -type f -print 2>/dev/null)
  for dir in ${tmp_dirs[@]+"${tmp_dirs[@]}"}; do
    rm -rf "$dir"
    removed=$((removed + 1))
  done
  # An empty `script/` keeps getting mistaken for `scripts/`. It holds nothing.
  if [[ -d "$repo_root/script" ]] && [[ -z "$(ls -A "$repo_root/script" 2>/dev/null)" ]]; then
    rmdir "$repo_root/script"
    removed=$((removed + 1))
  fi
  echo "$removed"
}

remove_incremental() {
  local freed=0
  while IFS= read -r dir; do
    freed=$(awk -v a="$freed" -v b="$(gb_of "$dir")" 'BEGIN {printf "%.1f", a + b}')
    rm -rf "$dir"
  done < <(find "$target_dir" -maxdepth 2 -type d -name incremental 2>/dev/null)
  echo "$freed"
}

# --guard runs after builds. It stays quiet unless the repo is over budget, and
# only ever drops caches that cost a rebuild of nothing that still exists.
if [[ "$guard" == true ]]; then
  while IFS= read -r dir; do
    [[ -n "$dir" ]] || continue
    dir_gb="$(gb_of "$dir")"
    if awk -v c="$dir_gb" -v b="$GUARD_DERIVED_BUDGET_GB" 'BEGIN {exit !(c > b)}'; then
      rm -rf "$dir"
      printf 'artifact guard: removed %s GB of Xcode DerivedData (budget %s GB). Xcode rebuilds it.\n' \
        "$dir_gb" "$GUARD_DERIVED_BUDGET_GB"
    fi
  done < <(plugapp_derived_data)

  current="$(gb_of "$target_dir")"
  over=$(awk -v c="$current" -v b="$GUARD_BUDGET_GB" 'BEGIN {print (c > b) ? 1 : 0}')
  if [[ "$over" == "1" ]]; then
    freed="$(remove_incremental)"
    litter_count="$(remove_litter)"
    printf 'artifact guard: target was %s GB (budget %s GB), freed %s GB of incremental cache and %s loose files\n' \
      "$current" "$GUARD_BUDGET_GB" "$freed" "$litter_count"
    remaining="$(gb_of "$target_dir")"
    if awk -v c="$remaining" -v b="$GUARD_BUDGET_GB" -v f="$GUARD_ESCALATE_FACTOR" 'BEGIN {exit !(c > b * f)}'; then
      printf 'artifact guard: still %s GB after reclaiming caches. Run scripts/clean-build-artifacts.sh --yes for a full reset.\n' "$remaining"
    fi
  fi
  exit 0
fi

if [[ "$confirm" != true && "$litter" != true && "$incremental" != true && "$xcode" != true && "$runtime_cache" != true ]]; then
  echo "Plug generated artifact cleanup"
  echo
  echo "Repo target:      $target_dir ($(size_of "$target_dir"))"
  while IFS= read -r dir; do
    [[ -n "$dir" ]] && echo "  incremental:    $dir ($(size_of "$dir"))"
  done < <(find "$target_dir" -maxdepth 2 -type d -name incremental 2>/dev/null)
  while IFS= read -r dir; do
    [[ -n "$dir" ]] && echo "Xcode DerivedData: $dir ($(size_of "$dir"))"
  done < <(plugapp_derived_data)
  echo "Runtime cache:    $artifact_cache ($(size_of "$artifact_cache"))"
  echo "Temporary dirs:   ${#tmp_dirs[@]}"
  for dir in ${tmp_dirs[@]+"${tmp_dirs[@]}"}; do
    echo "  - $dir ($(size_of "$dir"))"
  done
  echo
  echo "Dry run only. See --help for modes; --yes performs a full clean."
  exit 0
fi

if [[ "$confirm" == true ]]; then
  (cd "$repo_root" && cargo clean)
elif [[ "$incremental" == true ]]; then
  echo "Freed $(remove_incremental) GB of incremental cache."
fi

if [[ "$litter" == true ]]; then
  echo "Removed $(remove_litter) loose files and temporary directories."
fi

if [[ "$xcode" == true ]]; then
  while IFS= read -r dir; do
    [[ -n "$dir" ]] || continue
    echo "Removing $dir ($(size_of "$dir"))"
    rm -rf "$dir"
  done < <(plugapp_derived_data)
fi

if [[ "$runtime_cache" == true ]]; then
  rm -rf "$artifact_cache"
fi

echo
echo "Cleanup complete."
