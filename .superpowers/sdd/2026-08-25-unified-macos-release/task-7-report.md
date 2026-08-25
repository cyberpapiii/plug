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
