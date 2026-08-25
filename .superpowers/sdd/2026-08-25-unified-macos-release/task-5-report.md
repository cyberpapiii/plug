# Task 5 report: single-source app-only Homebrew Cask

## Result

The release workflow no longer embeds a second Cask template. The macOS app job
invokes `scripts/generate-release-metadata.sh --cask-only` after signing the
DMG, then verifies and uploads that generated `plug-app.rb`. The publish job
uses the same generator with `--formula-only`, preserving the uploaded Cask
instead of regenerating it.

The metadata generator now supports explicit Cask-only and Formula-only modes
while retaining its full-output mode. The contract test compares full-output
and Cask-only outputs byte-for-byte, compares full-output and Formula-only
outputs byte-for-byte, and rejects inline Cask DSL in the workflow.

## Runtime dependency

This release worktree intentionally does not add the `uninstall-cleanup`
runtime implementation. The command is supplied by the dependency worktree
`codex/unified-macos-install` (implementation lineage starts at `4d4cc84`,
current dependency head observed as `054eff6`). The Cask only carries the
app-owned command contract. The metadata test prints a deferred precondition
while this branch lacks the command; after the runtime merge it invokes
`cargo run --quiet -p plug-mcp -- uninstall-cleanup --help` as a safe parser
precondition. Final integration must exercise the real cleanup command against
the merged app runtime.

## Validation

- `bash -n scripts/generate-release-metadata.sh scripts/test-release-metadata.sh` — passed.
- `bash scripts/verify-release-workflow.sh` — passed.
- `bash scripts/test-release-metadata.sh` — passed; runtime precondition deferred pending dependency merge.
- Ruby syntax checks for generated Formula and Cask — passed.
- Ruby YAML parse of `.github/workflows/release.yml` — passed.
- `git diff --check` — passed.
- Homebrew local-path audit was unavailable: this installed Homebrew reports `Calling brew audit [path ...] is disabled`; Ruby syntax and repository contracts still passed.

## Concern

No release-branch runtime implementation was duplicated. Final release integration
still depends on merging the install/runtime worktree before proving actual
uninstall cleanup behavior.
