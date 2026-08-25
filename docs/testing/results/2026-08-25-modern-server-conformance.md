# MCP 2026-07-28 server conformance — 2026-08-25

Plug passed every active check in the pinned official prerelease server suite.

- Suite: `@modelcontextprotocol/conformance@0.2.0-alpha.10`
- Spec target: `2026-07-28`
- Direction: independent modern client to Plug's modern downstream HTTP adapter
- Upstream: checked-in `--official-modern-fixture` over stdio
- Result: **22 passed, 0 failed** across 20 scenarios

The run covered discovery metadata, completion, tool catalogs and calls,
text/image/audio/embedded/mixed content, errors, request-scoped progress over a
finite SSE response, multiple SSE streams, static and templated resources,
prompts, and DNS rebinding protection.

Command:

```sh
PLUG_MCP_CONFORMANCE_URL=http://127.0.0.1:19482/mcp \
  PLUG_MCP_CONFORMANCE_RESULTS_DIR=/tmp/plug-modern-conformance-results-final \
  scripts/check-mcp-conformance.sh official-modern-server
```

This is reproducible implementation evidence, not a stable certification: the
official 2026 suite is still prerelease, and the content fixture is part of this
repository. Plug therefore keeps both modern gates default-off and enables them
only for peers that have been tested.
