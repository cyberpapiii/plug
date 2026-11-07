# Sourced by the scripts that build. Cargo never deletes what it built for
# older versions of the source, so the target directory only grows: it reached 159 GB.
# When it passes the limit this clears it, and the build that follows takes
# a few minutes longer once.

trim_build_cache() {
  local limit_gb=40 used_kb dir="${CARGO_TARGET_DIR:-target}"
  [[ -d "$dir" ]] || return 0
  used_kb="$(du -sk "$dir" 2>/dev/null | cut -f1)"
  if ((${used_kb:-0} > limit_gb * 1024 * 1024)); then
    echo "build cache: $dir is over ${limit_gb} GB, clearing it"
    cargo clean --quiet
  fi
}
