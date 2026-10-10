# Plug.app performance baseline

How the app feels, in numbers. Re-run with `./scripts/perf-app.sh` (about
8 minutes) and `./scripts/perf-app.sh soak 30` (30 minutes), on the installed
`/Applications/Plug.app`, with the Mac left alone.

How to read it:

- Plug opens in the menu bar with no window, so "first usable window" is the
  "Open the window" rows.
- "Launch" is quit and open again with a warm disk cache, not a boot.
- Freezes and dropped frames are seen through accessibility pings, which add
  their own load. The same load is in every run, so compare runs with each
  other, not with a stopwatch.
- The build measured is a local `dev-install.sh` build.

## Before

| Measure | Plug 0.11.0, build 1791660101 |
| --- | --- |
| Size: Plug.app | 23.9 MB |
| Size: the app itself | 13.8 MB |
| Size: the daemon inside it | 7.1 MB |
| Size: the updater (Sparkle) | 2.8 MB |
| Launch: menu bar icon shows | 198 ms typical, 203 ms worst of 3 |
| Open the window: on screen | 492 ms typical, 562 ms worst of 3 |
| Open the window: usable | 615 ms typical, 715 ms worst of 3 |
| Idle CPU, app, window open | 8.57 % |
| Idle wakeups, app, window open | 75.9 per second |
| Memory, app, window open | 305 MB |
| Idle CPU, daemon, window open | 0.51 % |
| Idle wakeups, daemon, window open | 4.0 per second |
| Memory, daemon, window open | 26 MB |
| Idle CPU, app, window closed | 0.45 % |
| Idle wakeups, app, window closed | 1.4 per second |
| Memory, app, window closed | 336 MB |
| Idle CPU, daemon, window closed | 0.12 % |
| Idle wakeups, daemon, window closed | 4.0 per second |
| Memory, daemon, window closed | 26 MB |
| Switch section | 699 ms typical, 899 ms worst; longest freeze 86 ms typical, 228 ms worst; 7 dropped frames typical, 15 worst |
| Select a server | 342 ms typical, 905 ms worst; longest freeze 78 ms typical, 651 ms worst; 6 dropped frames typical, 42 worst |
| Select the largest server | 671 ms typical, 787 ms worst; longest freeze 458 ms typical, 481 ms worst; 38 dropped frames typical, 40 worst |
| Hide or show the sidebar | 850 ms typical, 899 ms worst; longest freeze 62 ms typical, 159 ms worst; 7 dropped frames typical, 17 worst |
| Resize the window, 24 steps on the largest server | 37 ms blocked per step, longest 47 ms, 35 dropped frames in all |
| Scroll the largest server, 21 steps | 24 ms blocked per step, longest 125 ms, 18 dropped frames in all |
| Open Settings | 627 ms typical, 787 ms worst; longest freeze 86 ms typical, 190 ms worst; 5 dropped frames typical, 11 worst |
| Open the menu bar panel | 869 ms typical, 881 ms worst; longest freeze 102 ms typical, 102 ms worst; 6 dropped frames typical, 6 worst |
| Memory after 30 minutes of use, app | 383 MB (started at 408, peak 409) |
| Memory after 30 minutes of use, daemon | 26 MB (started at 26) |
