#!/usr/bin/env bash
# Measure how the installed Plug.app feels: launch, idle cost, memory, size,
# and how long the main thread is held by the things people do most.
#
#   scripts/perf-app.sh             everything but the long session, about 8 min
#   scripts/perf-app.sh launch      quit and open Plug three times
#   scripts/perf-app.sh idle        CPU, wakeups and memory at rest
#   scripts/perf-app.sh actions     click through the window
#   scripts/perf-app.sh soak 30     use the window for 30 minutes, report memory
#   scripts/perf-app.sh size        what the bundle weighs
#
# It prints rows for the table in PERF_BASELINE.md. Leave the Mac alone while
# it runs: it quits and opens Plug, and drives its window through
# accessibility, which the terminal running it must be allowed to use. It never
# starts, stops, or restarts the daemon; quitting the app leaves it running.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
app="/Applications/Plug.app"
mode="${1:-all}"

size() {
  local total
  total="$(du -sk "$app" | cut -f1)"
  part() { printf '| Size: %s | %.1f MB |\n' "$1" "$(bc -l <<<"$(du -sk "$2" | cut -f1) / 1024")"; }
  printf '| Size: Plug.app | %.1f MB |\n' "$(bc -l <<<"$total / 1024")"
  part "the app itself" "$app/Contents/MacOS/Plug"
  part "the daemon inside it" "$app/Contents/Resources/plug"
  part "the updater (Sparkle)" "$app/Contents/Frameworks"
}

echo "| Measure | Plug $(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist"), build $(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$app/Contents/Info.plist") |"
echo "| --- | --- |"
[[ "$mode" == size || "$mode" == all ]] && size
[[ "$mode" == size ]] && exit 0

probe="${TMPDIR:-/tmp}/plug-perf-app"
if [[ ! -x "$probe" || "$repo_root/scripts/lib/perf-app.swift" -nt "$probe" ]]; then
  swiftc -O "$repo_root/scripts/lib/perf-app.swift" -o "$probe"
fi
daemon="$(launchctl print "gui/$(id -u)/com.plug.daemon" 2>/dev/null | awk '$1 == "pid" {print $3; exit}')"
exec "$probe" "${daemon:-0}" "$mode" "${@:2}"
