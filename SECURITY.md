# Security Policy

## Supported Versions

Plug is pre-1.0. Security fixes are made on `main` and in the latest published release line once releases are public.

## Reporting A Vulnerability

Do not file a public issue for vulnerabilities, credential exposure, auth bypasses, sandbox escapes, or unsafe upstream execution behavior.

Use GitHub private vulnerability reporting once the public repository is available:

https://github.com/cyberpapiii/plug/security/advisories/new

If private reporting is not enabled yet, contact the maintainer privately before publishing details. Include:

- A short impact summary.
- Affected Plug version or commit.
- Reproduction steps.
- Whether secrets, local files, or remote MCP credentials are exposed.
- Any logs or traces needed to verify the issue, with secrets redacted.

## Security Model

Plug is a multiplexer. It sits between trusted downstream clients and configured upstream MCP servers.

Important boundaries:

- Plug executes configured stdio commands. Treat `~/.config/plug/config.toml` as trusted operator input.
- Remote HTTP exposure must use TLS and auth. Plug rejects non-loopback binds without TLS.
- Downstream MCP auth and operator auth are separate. Operator tokens are administrative secrets.
- Upstream OAuth credentials and bearer tokens must not be committed to config files. Prefer environment variables or the credential store.
- Tool annotations are risk signals, not a security boundary. Operators should inspect `plug tools --output json` for upstream-declared versus Plug-inferred risk.
- Stdio sandboxing is opt-in and currently enforced on macOS only.

## Handling Secrets In Reports

Redact:

- OAuth access tokens and refresh tokens.
- Bearer tokens and operator tokens.
- API keys in server environment variables.
- Local filesystem paths that should not be disclosed publicly.

Keep enough structure for maintainers to reproduce the problem.
