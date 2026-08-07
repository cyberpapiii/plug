# MCP compatibility and conformance evidence

This inventory separates behavior that Plug proves in its own test suite from
behavior that has been exercised against an independent MCP implementation.
It deliberately does not describe local integration tests as official MCP
conformance.

The machine-readable inventory is
[`mcp-compatibility-inventory.tsv`](mcp-compatibility-inventory.tsv). Its status
terms mean:

- `proven-local`: a checked-in Plug test exercises the named behavior.
- `dormant`: Plug intentionally suppresses the path because it cannot yet
  translate it faithfully.
- `unavailable-external`: the required external peer or adapter was not
  available, so no result exists.
- `observed-peer`: an installed program and version were observed, but its MCP
  protocol support was not inferred from that version.

These labels are scoped to each row's surface. A proven tools call does not
prove OAuth, resources, prompts, subscriptions, tasks, or every transport in
the same protocol pair.

## Reproducing the evidence

Run the inventory check, which is fast and makes no network calls:

```sh
scripts/check-mcp-conformance.sh inventory
```

Run the focused repository tests behind the `proven-local` rows:

```sh
scripts/check-mcp-conformance.sh local
```

`local` first lists the relevant Cargo test targets and fails if any configured
selector matches zero tests. It also verifies that every `proven-local`
inventory evidence selector maps to the local run. The selector guard has a
fast deterministic check that does not compile or run the Rust suite:

```sh
scripts/check-mcp-conformance.sh self-test
```

The external conformance modes are intentionally not part of the default
repository gates. They never start, install, configure, or stop Plug. An
operator must separately start a disposable HTTP endpoint with the intended
configuration and explicitly supply its URL:

```sh
PLUG_MCP_CONFORMANCE_URL=http://127.0.0.1:3000/mcp \
  scripts/check-mcp-conformance.sh official-legacy-server

PLUG_MCP_CONFORMANCE_URL=http://127.0.0.1:3000/mcp \
  scripts/check-mcp-conformance.sh official-modern-server
```

The script pins `@modelcontextprotocol/conformance` exactly. On 2026-08-04,
the npm `latest` tag was `0.1.16`; the 2026-capable line was the prerelease
`0.2.0-alpha.10`. The modern mode therefore remains experimental evidence and
must not be reported as a stable-suite certification. `npx` may download the
pinned package into its normal cache when an external mode is explicitly run.

Results are written outside the repository by default and their location is
printed. Set `PLUG_MCP_CONFORMANCE_RESULTS_DIR` to choose a durable directory.
The operator must preserve the suite version, command, Plug commit, endpoint
configuration, and generated `checks.json` files before changing an
`unavailable-external` inventory row.

## Current four-way protocol matrix

| Downstream | Upstream | Current evidence | Product posture |
| --- | --- | --- | --- |
| Legacy | Legacy | Broad local regression coverage | Supported compatibility path |
| Legacy | Modern | Every `tools/call` is rejected before upstream effect | Dormant, fail closed; no cross-era call adapter |
| Modern | Legacy | Ordinary tools and local task wrapping proven locally | Partial adapter, opt-in |
| Modern | Modern | Discovery, sessionless catalogs, ordinary tools, and MRTR proven locally | Native path, opt-in, awaiting independent conformance |

No installed Claude, Cursor, or Codex version is marked modern-capable here.
Installed-version strings do not prove which protocol a real connection
requested or negotiated. That claim requires a captured connection or an
independent conformance run.

The same boundary applies upstream. A non-secret read of the installed Plug
configuration on 2026-08-04 found 13 configured peers, all using the default
legacy protocol mode:

| Transport | Configured peer names |
| --- | --- |
| stdio | `figma`, `imcp`, `oura`, `slack` |
| HTTP | `context7`, `exa`, `imessage`, `krisp`, `notion`, `supabase`, `svelte`, `todoist`, `workspace` |

This proves configuration presence only. It does not prove that a peer was
reachable or which version a live connection negotiated.

## External-suite boundary

The official MCP project publishes
[`@modelcontextprotocol/conformance`](https://github.com/modelcontextprotocol/conformance).
Its server runner can test an already-running HTTP endpoint. Its client runner
expects a command that accepts the scenario server URL appended by the suite.
Plug does not yet expose that client-adapter contract, so upstream-facing
official client conformance remains explicitly unavailable rather than being
papered over with a custom local mock.

## Official modern content fixture (opt-in)

The modern npm suite (`0.2.0-alpha.10`) expects fixed `test_*` / `test://…`
catalog entries. An empty Plug multiplexer cannot satisfy those rows. For
operator-driven evidence runs, start the mock harness in fixture mode and
attach it as the sole upstream with tool prefixing disabled so suite names
pass through unchanged:

```sh
# Terminal A — fixture upstream (stdio)
cargo run -p plug-test-harness --bin mock-mcp-server -- --official-modern-fixture

# Terminal B — disposable Plug HTTP with modern gate on, enable_prefix=false,
# one stdio upstream pointing at that mock binary, then:
PLUG_MCP_CONFORMANCE_URL=http://127.0.0.1:<port>/mcp \
  scripts/check-mcp-conformance.sh official-modern-server
```

Fixture coverage today: simple/error/image/audio/embedded/mixed tools,
`test://static-text` / `test://static-binary`, simple + args + image +
embedded-resource prompts, and `completion/complete`. Rows that need
client reverse requests or mid-call notifications
(`test_elicitation`, `test_sampling`, `test_tool_with_logging`,
`test_tool_with_progress`, …) remain expected-fail until a reverse-request
fixture lands. The suite package is still alpha — never report a green
opt-in run as stable certification. Modern gates stay default-off.
