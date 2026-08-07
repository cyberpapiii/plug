# Fleet MCP list contracts

`mock-lists.json` freezes the mock server's stable `tools/list`,
`resources/list`, `resources/templates/list`, and `prompts/list` responses.
The checker masks only top-level JSON-RPC request IDs, so names, descriptions,
schemas, metadata, and list membership remain contract-visible.

Regenerate the snapshot intentionally from current mock behavior:

```sh
python3 scripts/fleet/contract.py regen
```

Check without rewriting:

```sh
./scripts/fleet-truth.sh contract
```

Review the full snapshot diff before committing a regeneration. Tool renames,
schema changes, and additions or removals are contract changes, not recorder
noise.
