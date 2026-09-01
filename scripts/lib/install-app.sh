#!/usr/bin/env bash
# Shared by dev-install.sh and install-release.sh: put a verified Plug.app
# bundle into /Applications and wait for its daemon to answer.
#
#   source scripts/lib/install-app.sh
#   install_app_bundle /path/to/staged/Plug.app 0.8.11
#
# Replacing the bundle is the one destructive step. The swap moves the old
# bundle aside rather than deleting it, because every MCP client's
# `plug connect` re-execs the bundle's binary and macOS aborts a running process
# whose signed executable is unlinked underneath it. The app itself notices the
# new build number and replaces the daemon from inside the login session, which
# is the only place a daemon may be started from.

install_app_bundle() {
  local staged_app="$1"
  local version="$2"
  local app="/Applications/Plug.app"
  # Retired bundles wait here for their last clients to exit. Outside
  # /Applications so a superseded copy never shows up in Spotlight or Launchpad.
  local attic="${TMPDIR:-/tmp}/plug-retired-bundles"

  if pgrep -qf "$app/Contents/MacOS/Plug"; then
    echo "install: quitting the running Plug.app"
    osascript -e 'tell application "Plug" to quit' >/dev/null 2>&1 || true
    for _ in $(seq 1 30); do
      pgrep -qf "$app/Contents/MacOS/Plug" || break
      sleep 1
    done
    pgrep -qf "$app/Contents/MacOS/Plug" &&
      { echo "install: Plug.app is still running; not replacing it" >&2; return 1; }
  fi

  # A hand-installed LaunchAgent for the daemon predates the app-owned era and
  # would keep launchd pointed at whatever binary it names. The app registers
  # the daemon through SMAppService, so leaving one behind means two owners.
  local legacy_plist="$HOME/Library/LaunchAgents/com.plug.daemon.plist"
  if [[ -f "$legacy_plist" ]]; then
    echo "install: removing the CLI-owned LaunchAgent"
    launchctl bootout "gui/$(id -u)/com.plug.daemon" 2>/dev/null || true
    rm -f "$legacy_plist"
  fi

  echo "install: replacing $app"
  local incoming="/Applications/.Plug.app.incoming.$$"
  local retired="$attic/Plug.app.$(date +%Y%m%d-%H%M%S)"
  rm -rf "$incoming"
  ditto "$staged_app" "$incoming"
  if [[ -d "$app" ]]; then
    mkdir -p "$attic"
    mv "$app" "$retired"
  fi
  mv "$incoming" "$app"

  open -a "$app"

  # Yesterday's retired bundles have no processes left to protect.
  find "$attic" -maxdepth 1 -name 'Plug.app.*' -mtime +0 -exec rm -rf {} + 2>/dev/null || true

  # The old daemon keeps answering on the socket until the app replaces it,
  # so a bare status check would pass against the retired build. Wait for the
  # launchd job to carry the new bundle's build number.
  local build
  build="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$app/Contents/Info.plist")"
  echo "install: waiting for the daemon from build $build"
  local socket="$HOME/Library/Application Support/plug/plug.sock"
  for _ in $(seq 1 120); do
    if launchctl print "gui/$(id -u)/com.plug.daemon" 2>/dev/null |
        grep -qE "parent bundle version = $build\b" &&
      [[ -S "$socket" ]] && "$app/Contents/Resources/plug" status >/dev/null 2>&1; then
      echo "install: Plug $version (build $build) is running"
      return 0
    fi
    sleep 1
  done

  echo "install: Plug $version is installed, but the daemon did not answer within 120s." >&2
  echo "install: check $HOME/Library/Logs/plug" >&2
  return 1
}
