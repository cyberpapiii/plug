#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat >&2 <<'EOF'
usage: generate-release-metadata.sh --version <X.Y.Z> --dmg-sha <sha> \
  --linux-arm-sha <sha> --linux-x64-sha <sha> --output <dir>
EOF
    exit 2
}

VERSION=""
DMG_SHA=""
LINUX_ARM_SHA=""
LINUX_X64_SHA=""
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
        --linux-arm-sha)
            [[ $# -ge 2 ]] || usage
            LINUX_ARM_SHA="$2"
            shift 2
            ;;
        --linux-x64-sha)
            [[ $# -ge 2 ]] || usage
            LINUX_X64_SHA="$2"
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

[[ -n "$VERSION" && -n "$DMG_SHA" && -n "$LINUX_ARM_SHA" && \
   -n "$LINUX_X64_SHA" && -n "$OUTPUT" ]] || usage

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "error: version must match X.Y.Z: $VERSION" >&2
    exit 1
fi

for name in DMG_SHA LINUX_ARM_SHA LINUX_X64_SHA; do
    value="${!name}"
    if [[ ! "$value" =~ ^[0-9A-Fa-f]{64}$ ]]; then
        echo "error: $name must be a 64-character SHA-256 digest" >&2
        exit 1
    fi
done

mkdir -p "$OUTPUT"

RELEASE_TAG="v${VERSION}"
RELEASE_BASE_URL="https://github.com/cyberpapiii/plug/releases/download/${RELEASE_TAG}"

cat > "$OUTPUT/plug.rb" <<EOF
class Plug < Formula
  desc "MCP multiplexer - one config, every AI client connected, every server shared"
  homepage "https://github.com/cyberpapiii/plug"
  version "${VERSION}"
  license "Apache-2.0"

  depends_on :linux

  on_linux do
    if Hardware::CPU.arm?
      url "${RELEASE_BASE_URL}/plug-mcp-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "${LINUX_ARM_SHA}"
    else
      url "${RELEASE_BASE_URL}/plug-mcp-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "${LINUX_X64_SHA}"
    end
  end

  def install
    bin.install "plug"
  end

  test do
    assert_match "plug ${VERSION}", shell_output("#{bin}/plug --version")
  end
end
EOF

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
