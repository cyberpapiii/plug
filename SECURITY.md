# Security Policy

## Reporting a vulnerability

Report suspected vulnerabilities privately through GitHub's
[private vulnerability reporting](https://github.com/cyberpapiii/plug/security/advisories/new)
for this repository. Please do not open a public issue for a security problem.

Include the version or commit, your configuration shape (transport, `auth_mode`,
whether the downstream HTTP listener is exposed beyond loopback), and the
smallest reproduction you have. A proof-of-concept request is more useful than a
description of one.

Expect an acknowledgement within a week. plug is maintained by one person as a
personal project, so there is no paid support channel and no guaranteed
remediation window. Reports that identify a real weakness will be fixed and
credited in the release notes unless you ask otherwise.

## Supported versions

Only the latest release on `main` receives security fixes. There are no
long-term support branches.

## What plug is, in security terms

plug is a local MCP multiplexer. It holds credentials for upstream MCP servers
and, optionally, acts as an OAuth 2.1 authorization server for downstream MCP
clients. Two parts of it are security-relevant:

**Upstream credentials.** OAuth tokens for upstream servers are stored in the
operating system keychain where one is available, with a `0600` file mirror used
to survive restarts. Anyone who can read your user account can read those
credentials; plug does not defend against a compromised local account.

**The downstream authorization server.** When `http.auth_mode = "oauth"`, plug
issues tokens to remote MCP clients. Clients register through RFC 7591 Dynamic
Client Registration or present a Client ID Metadata Document. Every
authorization requires PKCE (S256 only), an exact redirect-URI match, a
resource-bound token per RFC 8707, and an explicit approval from the instance
owner authenticated with a WebAuthn passkey. Tokens carry method-family scopes
that are enforced on every request.

## Threat model

plug is designed to be safe to expose on a public origin behind a tunnel or
reverse proxy, with the owner-passkey ceremony as the gate that keeps strangers
from minting tokens.

In scope, and treated as vulnerabilities:

- Any way to obtain a token without an owner approval.
- Any way for one client's grant to reach another client's session, tokens, or
  data.
- Any way for a token to exercise a method family outside its granted scopes.
- Server-side request forgery through Client ID Metadata Document fetching,
  upstream URLs, or any other client-influenced request.
- Credential disclosure through logs, error responses, or status output.
- Bypassing the WebAuthn origin, user-verification, or consent-binding checks.

Out of scope:

- A compromised local user account. plug's credential storage is only as strong
  as the account that owns the keychain.
- Malicious or compromised upstream MCP servers. plug forwards their content and
  does not sandbox them; treat every configured upstream as trusted code.
- Denial of service from an authenticated client, unless it is disproportionate
  to the request that caused it.
- Running with `http.auth_mode = "none"` on a non-loopback address. The config
  validator rejects this, and overriding it is a deliberate choice to serve
  without authentication.
- Findings that require the operator to configure something the documentation
  explicitly warns against.

## Known gaps

Security work in progress is tracked in the repository's `todos/` directory.
Where a weakness is known and not yet fixed, it is written down rather than left
implicit. If you find something that is already tracked, the report is still
welcome; a second opinion on severity is useful.
