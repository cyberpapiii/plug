#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERIFY="$ROOT/scripts/verify-release-contract.sh"
WORKSPACE_VERSION="$(cargo metadata --manifest-path "$ROOT/Cargo.toml" --no-deps --format-version 1 \
  | python3 -c 'import json,sys; data=json.load(sys.stdin); print(next(p["version"] for p in data["packages"] if p["name"] == "plug-mcp"))')"
FIXTURE_ROOT="$(mktemp -d)"
trap 'rm -rf "$FIXTURE_ROOT"' EXIT

APP="$FIXTURE_ROOT/Plug.app"
APPCAST="$FIXTURE_ROOT/appcast.xml"
CASK="$FIXTURE_ROOT/plug-app.rb"
mkdir -p "$APP/Contents/Resources"

write_app() {
  local short_version="$1"
  local build_number="$2"
  cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleShortVersionString</key><string>${short_version}</string>
  <key>CFBundleVersion</key><string>${build_number}</string>
</dict></plist>
EOF
}

write_binary() {
  local version="$1"
  cat > "$APP/Contents/Resources/plug" <<EOF
#!/usr/bin/env bash
echo "plug ${version}"
EOF
  chmod +x "$APP/Contents/Resources/plug"
}

write_appcast() {
  local current_version="$1"
  local current_build="$2"
  local previous_build="$3"
  cat > "$APPCAST" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <item>
      <title>Plug ${current_version}</title>
      <sparkle:version>${current_build}</sparkle:version>
      <sparkle:shortVersionString>${current_version}</sparkle:shortVersionString>
      <enclosure url="https://example.invalid/Plug-${current_version}.dmg" />
    </item>
    <item>
      <title>Plug previous</title>
      <sparkle:version>${previous_build}</sparkle:version>
      <sparkle:shortVersionString>0.6.3</sparkle:shortVersionString>
      <enclosure url="https://example.invalid/Plug-previous.dmg" />
    </item>
  </channel>
</rss>
EOF
}

write_current_only_appcast() {
  local current_version="$1"
  local current_build="$2"
  cat > "$APPCAST" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <item>
      <title>Plug ${current_version}</title>
      <sparkle:version>${current_build}</sparkle:version>
      <sparkle:shortVersionString>${current_version}</sparkle:shortVersionString>
      <enclosure url="https://example.invalid/Plug-${current_version}.dmg" />
    </item>
  </channel>
</rss>
EOF
}

write_cask() {
  local version="$1"
  cat > "$CASK" <<EOF
cask "plug-app" do
  version "${version}"
  sha256 "fixture"
  app "Plug.app"
end
EOF
}

reset_valid_fixture() {
  write_app "$WORKSPACE_VERSION" 200
  write_binary "$WORKSPACE_VERSION"
  write_appcast "$WORKSPACE_VERSION" 200 199
  write_cask "$WORKSPACE_VERSION"
}

verify() {
  bash "$VERIFY" --tag "v$1" --app "$APP" --appcast "$APPCAST" --cask "$CASK"
}

verify_bootstrap() {
  bash "$VERIFY" --tag "v$1" --app "$APP" --appcast "$APPCAST" --cask "$CASK" \
    --allow-no-history
}

expect_failure() {
  local name="$1"
  local expected="$2"
  shift 2
  local output
  if output="$("$@" 2>&1)"; then
    echo "FAIL: $name unexpectedly passed" >&2
    exit 1
  fi
  if [[ "$output" != *"$expected"* ]]; then
    echo "FAIL: $name returned wrong error" >&2
    echo "expected: $expected" >&2
    echo "actual: $output" >&2
    exit 1
  fi
  echo "PASS: $name"
}

reset_valid_fixture
verify "$WORKSPACE_VERSION" >/dev/null
echo "PASS: matching release contract"

reset_valid_fixture
expect_failure "tag mismatch" "tag version" verify 9.9.9

reset_valid_fixture
write_app 9.9.9 200
expect_failure "app short version mismatch" "CFBundleShortVersionString" verify "$WORKSPACE_VERSION"

reset_valid_fixture
write_binary 9.9.9
expect_failure "embedded binary mismatch" "embedded plug version" verify "$WORKSPACE_VERSION"

reset_valid_fixture
write_appcast 9.9.9 200 199
expect_failure "appcast version mismatch" "appcast short version" verify "$WORKSPACE_VERSION"

reset_valid_fixture
write_cask 9.9.9
expect_failure "Cask version mismatch" "Cask version" verify "$WORKSPACE_VERSION"

reset_valid_fixture
write_app "$WORKSPACE_VERSION" 199
write_appcast "$WORKSPACE_VERSION" 199 199
expect_failure "non-increasing build number" "greater than prior appcast build" verify "$WORKSPACE_VERSION"

reset_valid_fixture
write_current_only_appcast "$WORKSPACE_VERSION" 200
expect_failure "missing published history" "published predecessor history is required" verify "$WORKSPACE_VERSION"

verify_bootstrap "$WORKSPACE_VERSION" >/dev/null
echo "PASS: explicit first-release bootstrap"

echo "All release contract tests passed."
