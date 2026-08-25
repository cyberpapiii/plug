# Task 6 report: transactional release publication

## Result

The release transaction now publishes `checksums.sha256` alongside every asset, then verifies its exact remote copy and expected GitHub download URL before touching the Homebrew tap. Final success requires both a non-draft, non-prerelease release and an exact `releases/latest.tag_name == TAG` match.

`scripts/test-publish-release-transaction.sh` provides a deterministic local fixture. It uses a fake `gh` state store and a disposable bare tap repository whose hook fails the first push. The fixture proves a failed tap push leaves the previous latest release unchanged, a retry publishes the pending single Formula+Cask commit before promotion, and an identical retry creates no second tap commit.

## Validation

- `bash -n scripts/publish-release-transaction.sh scripts/test-publish-release-transaction.sh` — passed.
- `bash scripts/test-publish-release-transaction.sh` — passed; forced tap failure preserved `v0.1.0`, retry promoted `v9.9.9`, identical retry kept two tap commits total.
- `bash scripts/verify-release-workflow.sh` — passed.
- Ruby YAML parse of `.github/workflows/release.yml` — passed.
- `git diff --check` — passed.

## Environment limits

`shellcheck` and `actionlint` are not installed in this environment. No CI-produced `artifacts/appcast.xml` or `checksums.sha256` exists in the worktree, so artifact-level `xmllint` and checksum validation remain workflow checks; the deterministic fixture covers remote asset upload/download and checksum verification.
