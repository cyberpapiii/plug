# Plug 0.7.2

Plug 0.7.2 completes the unified macOS installation repair.

It fixes first-run reconciliation when the existing daemon is stopped. The app
now reads Doctor's valid inspection report, adopts the recognized installation,
and starts the app-owned daemon instead of stopping early.

No configuration, credentials, or user data are changed.
