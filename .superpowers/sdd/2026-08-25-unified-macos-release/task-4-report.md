# Task 4 report: Linux-only standalone distribution

## Result

Committed as `35adf8d build: make standalone distribution Linux-only`.

Standalone cargo-dist targets and Homebrew Formula URLs are Linux-only. Darwin
builds remain in the macOS app job solely as universal Plug.app inputs. The
shell installer refuses Darwin before network access and points operators to
the release DMG or `brew install --cask cyberpapiii/tap/plug-app`. The obsolete
standalone macOS signing helper was deleted.

`scripts/generate-release-metadata.sh` now generates the Linux Formula and
app-only Cask from version and artifact digests. Its contract is covered by
`scripts/test-release-metadata.sh`.

## TDD evidence

The contract test was written first. Before implementation it failed with:

```text
bash: .../scripts/generate-release-metadata.sh: No such file or directory
```

After implementation:

```text
All release metadata contract tests passed.
```

The same test proves no Darwin standalone archive references, preserves both
Darwin app-input builds, rejects Darwin installation before curl, and checks
DMG/Cask guidance.

## Validation

- `bash -n install.sh scripts/generate-release-metadata.sh scripts/test-release-metadata.sh` — passed.
- `bash scripts/test-release-metadata.sh` — passed.
- `bash scripts/test-release-contract.sh` — passed.
- `dist plan --no-local-paths --allow-dirty` — passed; manifest lists four Linux targets only.
- Ruby YAML parse of `.github/workflows/release.yml` — passed.
- `git diff --check` — passed.

## Initial concern (resolved in fix round 1)

At the initial task commit, exact `dist plan --no-local-paths` was nonzero because the pre-existing
hand-maintained release workflow differs from cargo-dist's generated workflow;
dist requests a full generated-file rewrite. The custom workflow was preserved
because it owns the Plug.app build, signing, Sparkle, and publication pipeline.

## Fix round 1

Committed as `fix(release): harden custom Linux-only distribution gate`.

Three important release-review findings fixed.

- `dist-workspace.toml` disables cargo-dist CI generation (`ci = []`) because
  checked-in workflow is intentionally custom. `scripts/verify-release-workflow.sh`
  validates Linux archive matrix, Darwin app-input builds, app signing,
  published cargo-dist installer, absence of standalone Darwin assets, and the
  no-`--allow-dirty` contract. Release workflow runs validator plus exact
  `dist plan --no-local-paths` gate before publish.
- `scripts/patch-dist-installer.sh` patches actual cargo-dist-generated
  `plug-mcp-installer.sh` before upload. Darwin guard runs before download or
  filesystem setup, directs users to DMG/Homebrew Cask. Metadata test now builds
  real cargo-dist global artifact, patches it, exercises Darwin refusal with
  network/mutation sentinels; source `install.sh` contract remains covered.
- `docs/RELEASING.md` now describes Plug.app-only macOS distribution and current
  app signing path; deleted standalone Darwin signing/archive claims removed.

Validation:

- `bash scripts/test-release-metadata.sh` — passed, including generated
  `plug-mcp-installer.sh` Darwin contract.
- `dist plan --no-local-paths` — passed exactly; manifest lists four Linux
  targets plus Linux/global installers only.
- `bash -n install.sh scripts/generate-release-metadata.sh
  scripts/patch-dist-installer.sh scripts/verify-release-workflow.sh
  scripts/test-release-metadata.sh` — passed.
- Ruby YAML parse of `.github/workflows/release.yml` — passed.
- `git diff --check` — passed.

Remaining concern: `actionlint` unavailable; YAML validation used Ruby parsing
plus explicit workflow contract validator.
