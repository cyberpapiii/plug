---
status: complete
priority: p2
issue_id: "007"
tags: [code-review, security]
dependencies: []
---

# Symlink attack possible on socket path before bind

## Problem Statement

TOCTOU race between `remove_file` and `UnixListener::bind`. An attacker who can write to the runtime directory could place a symlink after the stale socket is removed. Also, `sock_path.exists()` follows symlinks, and `connect_to_daemon()` follows symlinks.

## Findings

- **Source**: security-review
- **Location**: `daemon.rs:237-248` (bind), `daemon.rs:574-580` (connect)
- **Practical risk**: LOW since runtime dir is 0700 user-owned, but defense-in-depth matters

## Proposed Solutions

Use `std::fs::symlink_metadata()` (lstat) instead of `Path::exists()`. Abort if path is a symlink.
- **Effort**: Small (20 min)

## Acceptance Criteria

- [ ] Daemon refuses to bind if socket path is a symlink
- [ ] Client refuses to connect if socket path is a symlink
- [ ] Uses `symlink_metadata` instead of `exists()`
