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

## After

One run of `scripts/perf-app.sh` on the branch's last build. The 30 minute
session was not run again.

| Measure | Plug 0.11.0, build 1791669506 |
| --- | --- |
| Size: Plug.app | 24.5 MB |
| Size: the app itself | 14.4 MB |
| Size: the daemon inside it | 7.1 MB |
| Size: the updater (Sparkle) | 2.8 MB |
| Launch: menu bar icon shows | 192 ms typical, 202 ms worst of 3 |
| Open the window: on screen | 495 ms typical, 530 ms worst of 3 |
| Open the window: usable | 568 ms typical, 645 ms worst of 3 |
| Idle CPU, app, window open | 7.41 % |
| Idle wakeups, app, window open | 66.5 per second |
| Memory, app, window open | 300 MB |
| Idle CPU, daemon, window open | 0.59 % |
| Idle wakeups, daemon, window open | 3.9 per second |
| Memory, daemon, window open | 25 MB |
| Idle CPU, app, window closed | 0.33 % |
| Idle wakeups, app, window closed | 1.3 per second |
| Memory, app, window closed | 71 MB |
| Idle CPU, daemon, window closed | 0.47 % |
| Idle wakeups, daemon, window closed | 4.2 per second |
| Memory, daemon, window closed | 25 MB |
| Switch section | 259 ms typical, 869 ms worst; longest freeze 119 ms typical, 192 ms worst; 7 dropped frames typical, 15 worst |
| Select a server | 333 ms typical, 903 ms worst; longest freeze 103 ms typical, 605 ms worst; 6 dropped frames typical, 39 worst |
| Select the largest server | 632 ms typical, 641 ms worst; longest freeze 355 ms typical, 523 ms worst; 36 dropped frames typical, 36 worst |
| Hide or show the sidebar | 492 ms typical, 879 ms worst; longest freeze 50 ms typical, 56 ms worst; 6 dropped frames typical, 8 worst |
| Resize the window, 24 steps on the largest server | 24 ms blocked per step, longest 25 ms, 8 dropped frames in all |
| Scroll the largest server, 21 steps | 11 ms blocked per step, longest 14 ms, 0 dropped frames in all |
| Open Settings | 113 ms typical, 673 ms worst; longest freeze 101 ms typical, 186 ms worst; 6 dropped frames typical, 11 worst |
| Open the menu bar panel | 683 ms typical, 858 ms worst; longest freeze 101 ms typical, 102 ms worst; 6 dropped frames typical, 6 worst |

The "typical" time of a click swings between about 150 ms and about 700 ms
from run to run, because it includes waiting for the window to settle. A second
run of the click-through on this build read 127 ms for Switch section, 148 ms
for Select a server and 491 ms for the menu bar panel. The longest freeze is
the steadier number.

An earlier build of this branch (1791667863) had the menu bar icon redrawing
in a loop after the app had been opened and closed a few times, holding 12 to
35 % CPU. That fault was there before the branch. On this build the same run
reads 0.33 % with the window closed.
