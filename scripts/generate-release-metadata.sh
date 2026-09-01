#!/usr/bin/env bash
set -euo pipefail

usage() {
    echo "usage: generate-release-metadata.sh --version <X.Y.Z> --dmg-sha <sha> --output <dir>" >&2
    exit 2
}

VERSION=""
DMG_SHA=""
OUTPUT=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --version)
            [[ $# -ge 2 ]] || usage
            VERSION="$2"
            shift 2
            ;;
        --dmg-sha)
            [[ $# -ge 2 ]] || usage
            DMG_SHA="$2"
            shift 2
            ;;
        --output)
            [[ $# -ge 2 ]] || usage
            OUTPUT="$2"
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

[[ -n "$VERSION" && -n "$DMG_SHA" && -n "$OUTPUT" ]] || usage

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "error: version must match X.Y.Z: $VERSION" >&2
    exit 1
fi
if [[ ! "$DMG_SHA" =~ ^[0-9A-Fa-f]{64}$ ]]; then
    echo "error: --dmg-sha must be a 64-character SHA-256 digest" >&2
    exit 1
fi

mkdir -p "$OUTPUT"

cat > "$OUTPUT/plug-app.rb" <<EOF
cask "plug-app" do
  version "${VERSION}"
  sha256 "${DMG_SHA}"

  url "https://github.com/cyberpapiii/plug/releases/download/v#{version}/Plug-#{version}.dmg"
  name "Plug"
  desc "Calm macOS control surface for the Plug MCP multiplexer"
  homepage "https://github.com/cyberpapiii/plug"

  auto_updates true
  depends_on macos: ">= :sonoma"
  app "Plug.app"
  uninstall script: {
    executable: "#{appdir}/Plug.app/Contents/Resources/plug",
    args:       ["uninstall-cleanup"],
  }
  caveats "Open Plug once to finish command-line and background-service setup."
end
EOF
