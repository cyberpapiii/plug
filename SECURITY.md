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
clients. Three parts of it are security-relevant:

**The configuration file.** plug launches the stdio commands named in your
config, with your environment and your privileges. The config file is trusted
operator input, equivalent to a shell script you run yourself. Sandboxing for
stdio child processes is opt-in per server and is currently implemented only on
macOS.

**Upstream credentials.** OAuth tokens for upstream servers are stored in the
operating system keychain where one is available, with a `0600` file mirror used
to survive restarts. Anyone who can read your user account can read those
credentials; plug does not defend against a compromised local account.

**Verified macOS command delegation.** A standalone macOS `plug` command may
verify Plug.app immediately before re-executing its embedded command-line
binary. This accepts the normal same-user time-of-check/time-of-use boundary:
an attacker able to replace a verified app between signature verification and
`exec` already controls that user's application files. Plug nevertheless
verifies the app signature, bundle identifier, and Developer ID Team ID on each
delegation and never intentionally delegates to an unsigned or wrong-Team-ID
target.

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
- The risk metadata plug reports for upstream tools. Those annotations are
  advisory signals for the operator, partly self-declared by the upstream
  server; they are not an enforcement boundary and are not claimed to be one.

## Redacting your report

Strip access and refresh tokens, bearer and API credentials, upstream server
environment variables, and any local path you would rather not publish. Keep the
shape of the request: header names, scope strings, error codes, and timing tell
us far more than the secret values do.
