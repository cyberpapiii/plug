---
status: complete
priority: p3
issue_id: "020"
tags: [code-review, architecture]
dependencies: []
---

# Tracing subscriber initialized before command dispatch blocks daemon file logging

## Problem Statement

`main()` initializes stderr tracing before dispatching commands. The daemon's `setup_file_logging()` cannot install its own subscriber because `init()` can only be called once.

## Findings

- **Source**: architecture-review
- **Location**: `main.rs:110-117`, `daemon.rs:548-567`
- **Current workaround**: `setup_file_logging` is `#[allow(dead_code)]`

## Proposed Solutions

Defer subscriber initialization based on command: daemon uses file logging, all others use stderr.
- **Effort**: Medium

## Acceptance Criteria

- [ ] Daemon mode uses file-based logging (tracing-appender)
- [ ] All other commands use stderr logging
- [ ] No `#[allow(dead_code)]` on setup_file_logging
