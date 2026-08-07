# Fleet golden MCP transcripts

These small sessions exercise the checked-in `plug-test-harness` mock server
over stdio JSON-RPC. The golden stage replays each request, masks volatile
protocol fields (request, task, session, and progress IDs plus timestamps), and
requires an empty normalized diff.

Regenerate every fixture from the current mock behavior:

```sh
python3 scripts/fleet/golden.py record
```

Verify without rewriting:

```sh
./scripts/fleet-truth.sh golden
```

Review fixture diffs before committing a regeneration. A changed stable field
is a behavioral change, not recorder noise.
