# Task 7 report: active installation documentation

## Result

Updated the eight Task 7 documents so macOS installation stays Plug.app-owned,
Linux standalone paths remain explicit, and source development has one clear
order. Fresh source setup now runs `./scripts/setup-codesigning.sh` before
`./scripts/dev-reinstall.sh`; the installed development binary is invoked only
afterward as `PLUG_DEV=1 plug-dev`. Removed the stale bare
`PLUG_DEV=1 plug-dev codesign-setup` workflow from the README.

The scoped legacy-install scan leaves one intentional match:
`brew install cyberpapiii/tap/plug` in the README's Linux-only section. No
macOS split-install guidance remains in the Task 7 files.

## Validation

- Scoped legacy-install `rg` scan — passed; one explicit Linux-only Formula match.
- Scoped `plug-dev` and source-order scan — passed.
- Scoped Markdown local-link check across all eight files — passed.
- `git diff --check` on all Task 7 files — passed.

## Concern

No blocking concern. Pre-existing untracked SDD review artifacts were left
untouched.

## Active-surface repair

Updated `CONTRIBUTING.md` to use one source-development order:
`./scripts/setup-codesigning.sh`, then `./scripts/dev-reinstall.sh`, then
`PLUG_DEV=1 plug-dev status`. The production `plug` ownership note now matches
the isolated `plug-dev` install.

Updated `plug doctor` code-signing guidance to the same order and removed the
stale `codesign-setup` invocation. Added a unit test that pins the command
order and rejects that stale command.

Updated `scripts/setup-codesigning.sh` so a missing `plug-dev` prints only the
reinstall step; it no longer advertises a development invocation before the
binary exists. Added deterministic output coverage for both binary-present and
binary-absent paths. The release metadata source gate now sets `PLUG_DEV=1`
when it exercises the embedded `uninstall-cleanup` command.

Validation:

- `cargo test -p plug-core doctor::tests` — 38 passed.
- `bash -n scripts/setup-codesigning.sh scripts/test-setup-codesigning.sh scripts/test-release-metadata.sh scripts/dev-reinstall.sh` — passed.
- `bash scripts/test-setup-codesigning.sh` — passed.
- `bash scripts/test-release-metadata.sh` — passed; the uninstall-cleanup branch was deferred because this source checkout does not yet contain `UninstallCleanup`.
- Scoped active-surface `rg` scan and `git diff --check` — passed.
