# Sourced by the scripts that build. Cargo never deletes what it built for
# older versions of the source, so `target/` only grows: it reached 159 GB.
# When it passes the limit this clears it, and the build that follows takes
# a few minutes longer once.

trim_build_cache() {
  local limit_gb=40 used_kb
  [[ -d target ]] || return 0
  used_kb="$(du -sk target 2>/dev/null | cut -f1)"
  if ((${used_kb:-0} > limit_gb * 1024 * 1024)); then
    echo "build cache: target/ is over ${limit_gb} GB, clearing it"
    cargo clean --quiet
  fi
}
