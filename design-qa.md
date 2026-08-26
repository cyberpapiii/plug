# Plug.app 0.8.1 Design QA

Date: 2026-08-26  
Reference: [Liquid Glass screen set](https://claude.ai/code/artifact/f29e1993-f058-4b67-af3e-1c03c7a03815)  
Installed build: `Plug.app` 0.8.1 (27), public notarized DMG

## Result

Pass. The installed public build was inspected after a real app-owned cold
start, not only in Xcode or from screenshots. It settled without a setup or
reconciliation banner and showed all 13 enabled servers as running with 486
routed tools.

## Screens Reviewed

- Servers: compact icon-led toolbar, plain `Running` and `Off` grouping,
  transport explained as `Runs on this Mac` or `Remote server`, and tool totals
  aligned for scanning.
- Tools: searchable grouped catalog with real tool names and accessible on/off
  switches.
- Connections: installed-client linking, live sessions, and remote grants share
  one understandable surface without duplicating authorization elsewhere.
- Activity: calm empty state; real calls include tool, client, session, and
  upstream attribution when present.
- Settings: General, Service, and About; service state, restart, reload,
  checkup, settings/log files, updates, version, and quit have one home.

## Interaction And Accessibility Checks

- The four primary workspaces switch correctly in the installed app.
- Status never relies on color alone; rows expose text and accessibility labels.
- Protocol and installation jargon is absent from the normal healthy path.
- Service actions use the canonical daemon manager rather than spawning a child.
- Destructive or externally meaningful actions remain explicit.

## Deviations And Follow-up

- The original HTML artifact was no longer present on disk for a pixel-overlay
  comparison, so this pass compared the published reference and live installed
  surfaces manually.
- Activity correctly showed its empty state immediately after the fresh daemon
  start; routed-call correctness was certified separately through a real stdio
  MCP session.
- No release-blocking visual or interaction defect remains from this pass.
