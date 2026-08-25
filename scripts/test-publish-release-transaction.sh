#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
TRANSACTION="$SCRIPT_DIR/scripts/publish-release-transaction.sh"
FIXTURE_ROOT="$(mktemp -d /tmp/plug-release-transaction.XXXXXX)"
trap 'rm -rf "$FIXTURE_ROOT"' EXIT

TAG="v9.9.9"
REPO="fixture/plug"
ARTIFACTS_DIR="$FIXTURE_ROOT/artifacts"
GH_STATE="$FIXTURE_ROOT/gh-state"
FAKE_BIN="$FIXTURE_ROOT/bin"
FAKE_GH="$FAKE_BIN/gh"
TAP_REMOTE="$FIXTURE_ROOT/homebrew-tap.git"
TAP_DIR="$FIXTURE_ROOT/homebrew-tap"
PUSH_ATTEMPTS="$FIXTURE_ROOT/push-attempts"

mkdir -p "$ARTIFACTS_DIR" "$GH_STATE/releases" "$FAKE_BIN" "$FIXTURE_ROOT/home"
export HOME="$FIXTURE_ROOT/home"
export GIT_CONFIG_NOSYSTEM=1

cat > "$ARTIFACTS_DIR/Plug-9.9.9.dmg" <<'EOF'
deterministic Plug DMG fixture
EOF
cat > "$ARTIFACTS_DIR/appcast.xml" <<'EOF'
deterministic Plug appcast fixture
EOF
cat > "$ARTIFACTS_DIR/plug.rb" <<'EOF'
class PlugFormulaFixture
end
EOF
cat > "$ARTIFACTS_DIR/plug-app.rb" <<'EOF'
cask "plug-app" do
  version "9.9.9"
end
EOF
cat > "$ARTIFACTS_DIR/RELEASE_NOTES.md" <<'EOF'
deterministic release notes fixture
EOF
(cd "$ARTIFACTS_DIR" && sha256sum Plug-9.9.9.dmg appcast.xml plug.rb plug-app.rb > checksums.sha256)

cat > "$FAKE_GH" <<EOF
#!/usr/bin/env bash
set -euo pipefail

STATE="$GH_STATE"
REPO="$REPO"

release_dir() {
  printf '%s/releases/%s' "\$STATE" "\$1"
}

api_assets() {
  local tag="\$1"
  local assets_dir="\$(release_dir "\$tag")/assets"
  local assets='[]'
  local path name url
  for path in "\$assets_dir"/*; do
    [[ -f "\$path" ]] || continue
    name="\$(basename "\$path")"
    url="https://github.com/\$REPO/releases/download/\$tag/\$name"
    assets="\$(jq --arg name "\$name" --arg url "\$url" \
      '. + [{name: \$name, browser_download_url: \$url, state: "uploaded"}]' <<<"\$assets")"
  done
  jq -n --arg tag "\$tag" --argjson assets "\$assets" \
    '{tag_name: \$tag, assets: \$assets}'
}

action="\$1"
case "\$action" in
  release)
    sub="\$2"
    tag="\$3"
    dir="\$(release_dir "\$tag")"
    case "\$sub" in
      view)
        [[ -d "\$dir" ]] || { echo "release not found" >&2; exit 1; }
        jq -n \
          --argjson draft "\$(cat "\$dir/draft")" \
          --argjson prerelease "\$(cat "\$dir/prerelease")" \
          '{isDraft: \$draft, isPrerelease: \$prerelease}'
        ;;
      create)
        mkdir -p "\$dir/assets"
        printf 'false\n' > "\$dir/draft"
        printf 'true\n' > "\$dir/prerelease"
        ;;
      upload)
        mkdir -p "\$dir/assets"
        shift 3
        while [[ \$# -gt 0 ]]; do
          case "\$1" in
            --repo) shift 2 ;;
            --clobber) shift ;;
            *) cp "\$1" "\$dir/assets/\$(basename "\$1")"; shift ;;
          esac
        done
        ;;
      download)
        download_dir=""
        shift 3
        while [[ \$# -gt 0 ]]; do
          case "\$1" in
            --repo|--dir)
              [[ "\$1" == --dir ]] && download_dir="\$2"
              shift 2
              ;;
            --clobber) shift ;;
            *) shift ;;
          esac
        done
        [[ -n "\$download_dir" ]] || { echo "download directory missing" >&2; exit 1; }
        mkdir -p "\$download_dir"
        cp "\$dir/assets"/* "\$download_dir/"
        ;;
      edit)
        shift 3
        while [[ \$# -gt 0 ]]; do
          case "\$1" in
            --repo) shift 2 ;;
            --draft=false) printf 'false\n' > "\$dir/draft"; shift ;;
            --draft=true) printf 'true\n' > "\$dir/draft"; shift ;;
            --prerelease=false) printf 'false\n' > "\$dir/prerelease"; shift ;;
            --prerelease=true|--prerelease) printf 'true\n' > "\$dir/prerelease"; shift ;;
            --latest) printf '%s\n' "\$tag" > "\$STATE/latest"; shift ;;
            *) shift ;;
          esac
        done
        ;;
      *) echo "unsupported fake gh release command: \$sub" >&2; exit 1 ;;
    esac
    ;;
  api)
    shift
    [[ \$# -gt 0 && "\$1" != -* ]] || {
      echo "unsupported fake gh api option: \${1:-<missing>}" >&2
      exit 1
    }
    endpoint="\$1"
    shift
    [[ \$# -eq 0 ]] || {
      echo "unsupported fake gh api option: \$1" >&2
      exit 1
    }
    if [[ "\$endpoint" == "repos/\$REPO/releases/latest" ]]; then
      jq -n --arg tag "\$(cat "\$STATE/latest")" '{tag_name: \$tag}'
    elif [[ "\$endpoint" == repos/\$REPO/releases/tags/* ]]; then
      api_assets "\$(basename "\$endpoint")"
    else
      echo "unsupported fake gh api endpoint: \$endpoint" >&2
      exit 1
    fi
    ;;
  *) echo "unsupported fake gh command: \$action" >&2; exit 1 ;;
esac
EOF
chmod +x "$FAKE_GH"
printf 'v0.1.0\n' > "$GH_STATE/latest"

git -c init.defaultBranch=main init --bare "$TAP_REMOTE" >/dev/null
git -c init.defaultBranch=main init "$TAP_DIR" >/dev/null
git -C "$TAP_DIR" config user.name fixture
git -C "$TAP_DIR" config user.email fixture@example.invalid
git -C "$TAP_DIR" switch -c main >/dev/null
mkdir -p "$TAP_DIR/Casks"
printf 'old formula\n' > "$TAP_DIR/plug.rb"
printf 'old cask\n' > "$TAP_DIR/Casks/plug-app.rb"
git -C "$TAP_DIR" add plug.rb Casks/plug-app.rb
git -C "$TAP_DIR" commit -m 'fixture: initial tap' >/dev/null
git -C "$TAP_DIR" remote add origin "$TAP_REMOTE"
git -C "$TAP_DIR" push -u origin main >/dev/null

cat > "$TAP_REMOTE/hooks/pre-receive" <<EOF
#!/usr/bin/env bash
set -euo pipefail
attempt=0
if [[ -f "$PUSH_ATTEMPTS" ]]; then
  attempt="\$(cat "$PUSH_ATTEMPTS")"
fi
attempt=\$((attempt + 1))
printf '%s\n' "\$attempt" > "$PUSH_ATTEMPTS"
if [[ "\$attempt" -eq 1 ]]; then
  echo 'deterministic fixture: forced tap push failure' >&2
  exit 1
fi
EOF
chmod +x "$TAP_REMOTE/hooks/pre-receive"

run_transaction() {
  GH_BIN="$FAKE_GH" \
    GITHUB_REPOSITORY="$REPO" \
    HOME="$FIXTURE_ROOT/home" \
    bash "$TRANSACTION" \
      --tag "$TAG" \
      --repo "$REPO" \
      --artifacts-dir "$ARTIFACTS_DIR" \
      --notes-file "$ARTIFACTS_DIR/RELEASE_NOTES.md" \
      --tap-dir "$TAP_DIR"
}

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

assert_equal() {
  local name="$1"
  local expected="$2"
  local actual="$3"
  [[ "$actual" == "$expected" ]] || fail "$name: expected '$expected', got '$actual'"
}

assert_contains() {
  local name="$1"
  local haystack="$2"
  local needle="$3"
  [[ "$haystack" == *"$needle"* ]] || fail "$name: missing '$needle'"
}

if first_output="$(run_transaction 2>&1)"; then
  fail 'forced tap push failure unexpectedly passed'
fi
assert_contains 'forced push output' "$first_output" 'forced tap push failure'
assert_equal 'latest after failed tap push' 'v0.1.0' "$(cat "$GH_STATE/latest")"
initial_remote_head="$(git --git-dir "$TAP_REMOTE" rev-parse refs/heads/main)"
local_pending_head="$(git -C "$TAP_DIR" rev-parse HEAD)"
[[ "$local_pending_head" != "$initial_remote_head" ]] || fail 'failed push did not leave local tap commit pending'

run_transaction >/dev/null
assert_equal 'latest after retry' "$TAG" "$(cat "$GH_STATE/latest")"
[[ -f "$GH_STATE/releases/$TAG/assets/checksums.sha256" ]] || fail 'checksum manifest was not uploaded'
retry_remote_head="$(git --git-dir "$TAP_REMOTE" rev-parse refs/heads/main)"
[[ "$retry_remote_head" == "$local_pending_head" ]] || fail 'retry did not publish pending tap commit'
assert_equal 'tap commit count after retry' '2' \
  "$(git --git-dir "$TAP_REMOTE" rev-list --count refs/heads/main)"

run_transaction >/dev/null
assert_equal 'latest after identical retry' "$TAG" "$(cat "$GH_STATE/latest")"
assert_equal 'tap head after identical retry' "$retry_remote_head" \
  "$(git --git-dir "$TAP_REMOTE" rev-parse refs/heads/main)"
assert_equal 'tap commit count after identical retry' '2' \
  "$(git --git-dir "$TAP_REMOTE" rev-list --count refs/heads/main)"
assert_equal 'push attempts' '2' "$(cat "$PUSH_ATTEMPTS")"

printf 'v0.1.0\n' > "$GH_STATE/latest"
if latest_mismatch_output="$(run_transaction 2>&1)"; then
  fail 'latest-tag mismatch unexpectedly passed'
fi
assert_contains 'latest-tag mismatch output' "$latest_mismatch_output" 'is not latest'
printf '%s\n' "$TAG" > "$GH_STATE/latest"

echo 'Release transaction fixture passed: failed push preserves latest; retry promotes; identical retry adds no tap commit.'
