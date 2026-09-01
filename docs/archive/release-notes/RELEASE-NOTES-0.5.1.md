# Plug 0.5.1

Plug 0.5.1 makes the new Mac app dependable on first launch.

The app no longer crashes after asking macOS for notification permission. It
also recognizes old or stale background-service registrations, replaces them
with the copy bundled inside Plug.app, and restarts the daemon cleanly. Existing
servers, client links, OAuth grants, configuration, and activity stay in place.

The test suite is also fenced off from your real macOS background-service
registration, preventing an interrupted development run from leaving a stale
test daemon behind.

This is the recommended version for every Plug.app user. The CLI and protocol
features are unchanged from 0.5.0.
