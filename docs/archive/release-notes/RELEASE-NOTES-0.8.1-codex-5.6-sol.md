# Plug 0.8.1 — Reliable first launch

Plug 0.8.1 fixes a startup problem in the new Mac app. On machines with many
configured servers, Plug could mistake a normal cold start for a failed one and
repeatedly restart its background service before it became ready.

The app now starts the service once and gives it a bounded 90-second window to
finish connecting its servers. This removes the false “Setup incomplete” state
without allowing an actually stuck service to retry forever.

No configuration, credentials, connected apps, servers, or tools are changed by
this update.
