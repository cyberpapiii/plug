#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
usage: publish-release-transaction.sh \
  --tag <vX.Y.Z> \
  --artifacts-dir <directory> \
  --notes-file <RELEASE_NOTES.md> \
  --tap-dir <homebrew-tap> \
  [--repo <owner/repository>]
EOF
  exit 2
}

TAG=""
ARTIFACTS_DIR=""
NOTES_FILE=""
TAP_DIR=""
REPO="${GITHUB_REPOSITORY:-cyberpapiii/plug}"
GH_BIN="${GH_BIN:-gh}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag)
      [[ $# -ge 2 ]] || usage
      TAG="$2"
      shift 2
      ;;
    --artifacts-dir)
      [[ $# -ge 2 ]] || usage
      ARTIFACTS_DIR="$2"
      shift 2
      ;;
    --notes-file)
      [[ $# -ge 2 ]] || usage
      NOTES_FILE="$2"
      shift 2
      ;;
    --tap-dir)
      [[ $# -ge 2 ]] || usage
      TAP_DIR="$2"
      shift 2
      ;;
    --repo)
      [[ $# -ge 2 ]] || usage
      REPO="$2"
      shift 2
      ;;
    -h|--help)
      usage
      ;;
    *)
      echo "error: unknown option: $1" >&2
      usage
      ;;
  esac
done

[[ -n "$TAG" && "$TAG" == v* ]] || usage
[[ -n "$ARTIFACTS_DIR" && -d "$ARTIFACTS_DIR" ]] || usage
[[ -n "$NOTES_FILE" && -f "$NOTES_FILE" ]] || usage
[[ -n "$TAP_DIR" && -d "$TAP_DIR" ]] || usage

die() {
  echo "release transaction: $*" >&2
  exit 1
}

command -v "$GH_BIN" >/dev/null 2>&1 || die "GitHub CLI not found: $GH_BIN"
command -v jq >/dev/null 2>&1 || die "jq is required"
command -v sha256sum >/dev/null 2>&1 || die "sha256sum is required"
git -C "$TAP_DIR" rev-parse --git-dir >/dev/null 2>&1 || die "tap directory is not a git checkout: $TAP_DIR"

ARTIFACTS_DIR="$(cd "$ARTIFACTS_DIR" && pwd -P)"
NOTES_FILE="$(cd "$(dirname "$NOTES_FILE")" && pwd -P)/$(basename "$NOTES_FILE")"
TAP_DIR="$(cd "$TAP_DIR" && pwd -P)"
CHECKSUMS_FILE="$ARTIFACTS_DIR/checksums.sha256"

[[ -f "$CHECKSUMS_FILE" ]] || die "checksum manifest missing: $CHECKSUMS_FILE"

declare -a ASSET_NAMES=()
declare -a ASSET_PATHS=()

while read -r digest name extra; do
  [[ -n "${digest:-}" ]] || continue
  [[ -z "${extra:-}" && "$digest" =~ ^[[:xdigit:]]{64}$ ]] || \
    die "invalid checksum manifest line"
  [[ -n "${name:-}" && "$name" != */* && "$name" != /* ]] || \
    die "checksum manifest contains unsafe asset name: ${name:-<empty>}"
  if (( ${#ASSET_NAMES[@]} > 0 )); then
    for existing_name in "${ASSET_NAMES[@]}"; do
      [[ "$existing_name" != "$name" ]] || die "duplicate asset in checksum manifest: $name"
    done
  fi
  [[ -f "$ARTIFACTS_DIR/$name" ]] || die "asset missing: $ARTIFACTS_DIR/$name"
  ASSET_NAMES+=("$name")
  ASSET_PATHS+=("$ARTIFACTS_DIR/$name")
done < "$CHECKSUMS_FILE"

(( ${#ASSET_NAMES[@]} > 0 )) || die "checksum manifest has no assets"

echo "Verifying staged release assets"
(cd "$ARTIFACTS_DIR" && sha256sum -c "$(basename "$CHECKSUMS_FILE")")

gh_run() {
  "$GH_BIN" "$@"
}

release_json=""
release_error="$(mktemp "${TMPDIR:-/tmp}/plug-release-view.XXXXXX")"
release_exists=false
if release_json="$(gh_run release view "$TAG" --repo "$REPO" --json isDraft,isPrerelease 2>"$release_error")"; then
  release_exists=true
else
  if ! grep -Eiq 'not found|404' "$release_error"; then
    cat "$release_error" >&2
    rm -f "$release_error"
    die "could not inspect release $TAG"
  fi
fi
rm -f "$release_error"

is_draft=false
is_prerelease=true
release_was_published=false
if [[ "$release_exists" == true ]]; then
  is_draft="$(jq -r '.isDraft // false' <<<"$release_json")"
  is_prerelease="$(jq -r '.isPrerelease // false' <<<"$release_json")"
  [[ "$is_draft" == true || "$is_draft" == false ]] || die "invalid release draft state"
  [[ "$is_prerelease" == true || "$is_prerelease" == false ]] || die "invalid release prerelease state"
  if [[ "$is_prerelease" == false && "$is_draft" == false ]]; then
    release_was_published=true
    echo "Reusing published release $TAG; remote asset verification is read-only"
  elif [[ "$is_draft" == true ]]; then
    echo "Reusing draft release $TAG as prerelease"
    gh_run release edit "$TAG" --repo "$REPO" --draft=false --prerelease
    is_draft=false
    is_prerelease=true
  else
    echo "Reusing prerelease $TAG"
  fi
else
  echo "Creating prerelease $TAG"
  gh_run release create "$TAG" \
    --repo "$REPO" \
    --verify-tag \
    --title "plug $TAG" \
    --notes-file "$NOTES_FILE" \
    --prerelease
fi

if [[ "$release_was_published" == false ]]; then
  echo "Uploading complete asset set with --clobber"
  gh_run release upload "$TAG" --repo "$REPO" "${ASSET_PATHS[@]}" --clobber
fi

release_api="$(gh_run api --repo "$REPO" "repos/$REPO/releases/tags/$TAG")"
remote_asset_count="$(jq '.assets | length' <<<"$release_api")"
(( remote_asset_count >= ${#ASSET_NAMES[@]} )) || \
  die "release $TAG has $remote_asset_count assets; expected at least ${#ASSET_NAMES[@]}"

for asset_name in "${ASSET_NAMES[@]}"; do
  remote_asset="$(jq -r --arg name "$asset_name" '
    .assets[] | select(.name == $name) |
    [.name, (.browser_download_url // ""), (.state // "uploaded")] | @tsv
  ' <<<"$release_api")"
  [[ -n "$remote_asset" ]] || die "release asset missing: $asset_name"
  IFS=$'\t' read -r remote_name remote_url remote_state <<<"$remote_asset"
  expected_url="https://github.com/$REPO/releases/download/$TAG/$asset_name"
  [[ "$remote_name" == "$asset_name" ]] || die "release asset name mismatch: $asset_name"
  [[ "$remote_url" == "$expected_url" ]] || \
    die "release asset URL mismatch for $asset_name: $remote_url"
  [[ "$remote_state" == uploaded ]] || die "release asset is not uploaded: $asset_name"
done

REMOTE_VERIFY_DIR="$(mktemp -d "${TMPDIR:-/tmp}/plug-release-assets.XXXXXX")"
cleanup_remote_assets() {
  rm -rf "$REMOTE_VERIFY_DIR"
}
trap cleanup_remote_assets EXIT

echo "Downloading release assets for remote checksum verification"
gh_run release download "$TAG" --repo "$REPO" --dir "$REMOTE_VERIFY_DIR" --clobber
for asset_name in "${ASSET_NAMES[@]}"; do
  [[ -f "$REMOTE_VERIFY_DIR/$asset_name" ]] || die "downloaded release asset missing: $asset_name"
done
cmp -s "$CHECKSUMS_FILE" "$REMOTE_VERIFY_DIR/$(basename "$CHECKSUMS_FILE")" || \
  die "remote checksum manifest differs from staged manifest"
(cd "$REMOTE_VERIFY_DIR" && sha256sum -c "$(basename "$CHECKSUMS_FILE")")

FORMULA="$ARTIFACTS_DIR/plug.rb"
CASK="$ARTIFACTS_DIR/plug-app.rb"
[[ -f "$FORMULA" && -f "$CASK" ]] || die "Formula and Cask must be staged assets"
mkdir -p "$TAP_DIR/Casks"
cp "$FORMULA" "$TAP_DIR/plug.rb"
cp "$CASK" "$TAP_DIR/Casks/plug-app.rb"

tap_branch="$(git -C "$TAP_DIR" symbolic-ref --quiet --short HEAD)" || \
  die "tap checkout is not on a branch"
git -C "$TAP_DIR" config user.name "github-actions[bot]"
git -C "$TAP_DIR" config user.email "github-actions[bot]@users.noreply.github.com"
git -C "$TAP_DIR" add -- plug.rb Casks/plug-app.rb

if git -C "$TAP_DIR" diff --cached --quiet -- plug.rb Casks/plug-app.rb; then
  echo "Tap Formula+Cask already identical; skipping tap commit"
else
  git -C "$TAP_DIR" commit --only -m "plug ${TAG#v}" -- plug.rb Casks/plug-app.rb
  echo "Created one tap commit for Formula+Cask"
fi

tap_upstream="$(git -C "$TAP_DIR" rev-parse --abbrev-ref --symbolic-full-name '@{u}' 2>/dev/null || true)"
tap_ahead=0
if [[ -n "$tap_upstream" ]]; then
  tap_ahead="$(git -C "$TAP_DIR" rev-list --count "$tap_upstream..HEAD")"
else
  tap_ahead=1
fi

if (( tap_ahead > 0 )); then
  echo "Pushing tap commit"
  git -C "$TAP_DIR" push origin "$tap_branch"
else
  echo "Tap branch already up to date; skipping push"
fi

git -C "$TAP_DIR" fetch origin "$tap_branch" --quiet
tap_remote_ref="refs/remotes/origin/$tap_branch"
tap_remote_head="$(git -C "$TAP_DIR" rev-parse "$tap_remote_ref")"
tap_head="$(git -C "$TAP_DIR" rev-parse HEAD)"
git -C "$TAP_DIR" merge-base --is-ancestor "$tap_head" "$tap_remote_head" || \
  die "tap commit is not present on origin/$tap_branch"
echo "Tap commit verified on origin/$tap_branch: $tap_head"

if [[ "$release_was_published" == true ]]; then
  echo "Release $TAG already promoted; skipping promotion"
else
  echo "Promoting $TAG after tap verification"
  gh_run release edit "$TAG" --repo "$REPO" --draft=false --prerelease=false --latest
fi

final_json="$(gh_run release view "$TAG" --repo "$REPO" --json isDraft,isPrerelease)"
final_draft="$(jq -r '.isDraft // false' <<<"$final_json")"
final_prerelease="$(jq -r '.isPrerelease // false' <<<"$final_json")"
[[ "$final_draft" == false && "$final_prerelease" == false ]] || \
  die "release $TAG was not promoted"
echo "Release transaction complete: $TAG latest, tap commit $tap_head"
