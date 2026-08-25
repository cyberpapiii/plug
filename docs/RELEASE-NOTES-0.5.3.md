# Plug 0.5.3

Plug 0.5.3 makes the Mac app's operator window actually usable after the app
takes ownership of the background service.

The app now connects to Plug's local Unix socket using the correct macOS
address layout, then reads server, client, activity, and authentication data
without tripping over JSON field names such as server and session IDs.

In practical terms: the menu-bar app opens to your real server list instead of
showing `connect failed (47)` or an empty-data error. The daemon, CLI clients,
configuration, OAuth grants, and MCP behavior are unchanged.

This release includes real socket-connect and operator-snapshot regression
tests so both failures stay fixed.
