# Plug 0.7.5

Plug.app is now the only daemon owner on an installed Mac. Client reconnects
open the app for recovery instead of rebuilding the retired command-line
LaunchAgent, and external process supervisors can no longer start a competing
production daemon.

After first-run adoption has been approved once, Plug automatically repairs a
missing or overwritten service registration. Fresh installations still show
the original one-time adoption confirmation before replacing existing daemon
ownership.

These changes remove the ownership loop that could leave launchd stuck between
the app, command-line reconnects, and another process supervisor.
