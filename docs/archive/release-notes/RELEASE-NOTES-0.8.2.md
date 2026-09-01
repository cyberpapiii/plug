# Plug 0.8.2 — Faster starts, quieter app

Plug 0.8.2 is a performance and reliability release. Nothing about
configuration, credentials, connected apps, servers, or tools changes.

## One slow server no longer holds up the rest

Starting Plug means connecting every configured server. Two paths could let a
single unreachable host consume its entire start budget while every other
server waited behind it: an HTTP or legacy SSE upstream whose host never
answered, and OAuth metadata discovery, which ran with no connect bound and a
thirty-second overall timeout — exactly the default per-server start timeout.

Both are now bounded. Upstream HTTP and SSE clients use a ten-second connect
timeout, and discovery on the start path gets at most a sixth of that server's
own start budget. A recorded start took 32.65 seconds across thirteen servers;
twelve were up in under 1.7 seconds each and the thirteenth spent 30.18 seconds
inside discovery before failing.

## The menu bar app asks for far less

Plug.app polls the daemon every couple of seconds to show health, tool counts
and live sessions. It was also refetching the entire tool list on a timer and
renegotiating its handshake on every poll, and the daemon was including upstream
branding icons — base64 images — in every status snapshot.

The daemon now reports when the tool catalog would answer differently, so the
app fetches the large list only then; it reuses the handshake it already has;
and branding icons stay out of the status snapshot. Tool listings still carry
icons, which is where an app renders them. Thirty seconds of polling went from
roughly 1918 KiB across 45 round trips to roughly 262 KiB across 30.
