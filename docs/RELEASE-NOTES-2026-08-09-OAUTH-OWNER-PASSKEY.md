# Owner-Verified Downstream OAuth Release Candidate

Status: `exists off-main` on `codex/oauth-owner-passkey-implementation`.
Reviewed implementation head: `07c5c14`. This candidate has not been merged,
released, signed, or installed.

## What Changes

A remote client still begins with Plug's public `/mcp` URL and completes the
standard public-client OAuth flow. Before Plug issues a new authorization code,
the owner now approves or denies the exact client, redirect, resource, and scope
request with a passkey on Plug's public HTTPS origin. Authorization requests,
WebAuthn ceremonies, and completed decisions are durable across service
restarts, single-use, expiry-bound, and tied to the original consent details.

The owner enrolls and administers passkeys locally:

```sh
plug auth owner enroll
plug auth owner list
plug auth owner remove <credential-id>
```

Enrollment uses a single-use five-minute bootstrap delivered in a URL fragment.
Passkey private keys remain in the platform authenticator; Plug stores only the
public credential material and summary metadata. Up to five owner passkeys may
be enrolled. Removing the final credential requires explicit confirmation and
blocks new grants until another passkey is enrolled.

Local client revocation, owner administration, and live-session inventory use
short-lived, single-use HMAC proofs bound to the exact request. The reusable
operator token stays on the local machine and is not sent in HTTP requests,
URLs, logs, or command output.

Existing owner-only OAuth state migrates to the new schema without weakening
file ownership or write-before-publish behavior. Existing client grants remain
administrable, while new approvals require an enrolled owner passkey.

## Verification

Fresh implementation-head verification passed:

- 1,101 Rust workspace tests with one test thread
- formatting, clippy with warnings denied, current-toolchain workspace check,
  and Rust 1.88 MSRV workspace check
- `cargo deny` license, ban, source, and advisory policy
- Trivy high/critical dependency and secret scan, Semgrep security audit, and
  a zero-leak exact-branch Gitleaks scan
- `npm ci` with zero audited vulnerabilities
- 5 real-process Playwright tests; 1 WebKit platform-authenticator test skipped
  intentionally because Playwright WebKit has no virtual authenticator
- `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-gnu` workspace checks
- release binary size gate: 6,585,472 bytes on the reviewed macOS build, below
  the 10 MiB cap; the stable Rust 1.97 Linux CI-size build is 10,334,136 bytes,
  also below the cap

The Chromium browser proof covers enrollment, owner approval, PKCE token
exchange, authenticated MCP use, refresh rotation, restart recovery, and
revocation against the real Plug binary and an upstream MCP process. Chromium
and WebKit both cover the shared public HTTPS UI, exact-origin behavior,
security headers, denial, expiry, restart, error handling, and failure-log
redaction.

## Gates Still Pending

- a real WebKit platform-passkey ceremony against a signed build
- default-browser Touch ID verification on macOS against that signed build
- live end-to-end certification for each named client; Claude, ChatGPT, Codex,
  Cursor, and OpenCode are not certified by this candidate
- exact-head hosted CI, merge to `main`, signed packaging, installation, and
  post-install runtime verification

Until those gates pass, describe this work only as `exists off-main`, never as
released or done on `main`.
