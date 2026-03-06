# UX Design

`plug` currently serves two audiences:

- humans operating MCP infrastructure from a terminal
- AI agents calling commands programmatically

This document describes the current product phase: **guided CLI first, TUI later**.

---

## Product Stance

The CLI is the product surface right now.

That means:

- `plug` with no args should act like a home screen
- `plug clients`, `plug servers`, and `plug tools` should act like management views
- setup and linking should feel guided
- runtime inspection should be easy without reading docs
- plumbing commands should exist, but they should not dominate the story

The deferred TUI should not shape today’s command model.

---

## UX Principles

### 1. Useful By Default

The default invocation should help the user move forward.

- `plug` should show a compact overview and next actions
- no raw help dump as the default experience
- no assumption that the daemon is already running

### 2. Commands Map To Jobs

Top-level commands should represent what the user is trying to do.

Current human-facing jobs:

- `plug start`
- `plug setup`
- `plug clients`
- `plug link`
- `plug unlink`
- `plug status`
- `plug servers`
- `plug tools`
- `plug doctor`
- `plug repair`
- `plug config check`
- `plug config --path`

Internal/plumbing commands stay available:

- `plug connect`
- `plug serve`
- `plug stop`
- `plug reload`

### 3. Interactive When Helpful, Scriptable When Needed

Interactive flows are useful for humans, but the main paths must also work without prompts.

Examples:

- `plug setup --yes`
- `plug link claude-code cursor`
- `plug link --all`
- `plug import --yes`
- `plug status --output json`

### 4. Truthful Language

Use words that match the user’s mental model.

- Prefer “link clients” over “export config” in the human-facing story
- Prefer “overview” and “next actions” over “dashboard” when there is no dashboard
- Mark plumbing commands as internal or advanced

---

## Command Model

### Home Screen

```text
plug
```

Expected behavior:

- show config path
- show server count
- show linked client count
- show whether the background service is running
- show the next one or two useful actions

### Get Started

```text
plug setup
plug clients
plug link
```

`setup` is the concierge flow:

- discover existing MCP servers
- import them into `plug`
- hand off to linking clients

`link` is the focused client-linking flow:

- interactive by default
- direct targets supported for scripting

`clients` is the client state surface:

- linked vs detected vs live
- quick audit of what is actually connected
- interactive link/unlink from the same view

`servers` is the server management surface:

- health and tool counts at a glance
- add, edit, remove, enable, disable from the same view
- advanced schema editing can still fall back to config editing when needed

`tools` is the effective tool surface:

- grouped view of what clients can actually call
- disable or re-enable tools from the same view
- stable non-interactive commands remain available underneath

### Inspect

```text
plug status
plug clients
plug servers
plug tools
plug doctor
```

These commands should be the calm, reliable operating surface for the product.

The management view pattern should be consistent:

- banner
- summary
- inventory
- actions
- one `Choose action` prompt grammar across views

### Maintain

```text
plug repair
plug config check
plug config --path
```

These commands should fix drift and make local state understandable.

### Internal

```text
plug connect
plug serve --daemon
plug stop
plug reload
```

These are required, but they should be clearly described as plumbing.

---

## Help Output

`plug --help` should read like a product menu, not a transport manifest.

Recommended grouping:

- Get Started
- Inspect
- Maintain
- Internal

Short command descriptions should explain the user job, not the implementation detail.

Bad:

- “export config”
- “start transport”

Better:

- “link plug to your AI clients”
- “internal: start the stdio adapter AI clients invoke”

---

## Output Modes

### Human Output

Text output should be:

- compact
- legible
- action-oriented
- free of unnecessary implementation noise

### Agent Output

`--output json` should remain stable and useful for:

- status checks
- tool inspection
- diagnostics
- setup/import automation where supported

---

## Deferred Work

The following are intentionally deferred until the CLI model is fully settled:

- default TUI or dashboard behavior
- TUI-oriented navigation systems
- visual tab bars, mouse support, or panel metaphors
- product language that assumes a live terminal UI exists

When a TUI returns, it should be layered on top of a command model that is already coherent.
