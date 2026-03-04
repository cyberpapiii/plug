#!/usr/bin/env sh
# plug installer
# Usage: curl -fsSL https://get.plug.sh | sh
# Or:    curl -fsSL https://get.plug.sh | sh -s -- --install-dir ~/.local/bin

set -eu

PLUG_REPO="plug-mcp/plug"
PLUG_BIN="plug"
PLUG_INSTALL_DIR=""

# Colors (disabled if NO_COLOR is set or terminal doesn't support them)
if [ -z "${NO_COLOR:-}" ] && [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[1;33m'
    CYAN='\033[0;36m'
    RESET='\033[0m'
else
    RED=''
    GREEN=''
    YELLOW=''
    CYAN=''
    RESET=''
fi

info()    { printf "${CYAN}info${RESET}  %s\n" "$*"; }
success() { printf "${GREEN}ok${RESET}    %s\n" "$*"; }
warn()    { printf "${YELLOW}warn${RESET}  %s\n" "$*"; }
error()   { printf "${RED}error${RESET} %s\n" "$*" >&2; exit 1; }

# Parse arguments
while [ $# -gt 0 ]; do
    case "$1" in
        --install-dir)
            PLUG_INSTALL_DIR="$2"
            shift 2
            ;;
        --install-dir=*)
            PLUG_INSTALL_DIR="${1#*=}"
            shift
            ;;
        --version)
            PLUG_VERSION="$2"
            shift 2
            ;;
        --version=*)
            PLUG_VERSION="${1#*=}"
            shift
            ;;
        -h|--help)
            cat <<EOF
plug installer

USAGE:
    curl -fsSL https://get.plug.sh | sh [-- OPTIONS]

OPTIONS:
    --install-dir <dir>   Install to this directory (default: ~/.local/bin or /usr/local/bin)
    --version <version>   Install specific version (default: latest)
    -h, --help            Show this help
EOF
            exit 0
            ;;
        *)
            error "Unknown option: $1"
            ;;
    esac
done

# Detect OS
detect_os() {
    OS="$(uname -s)"
    case "$OS" in
        Linux)  echo "linux" ;;
        Darwin) echo "macos" ;;
        MINGW*|MSYS*|CYGWIN*) echo "windows" ;;
        *) error "Unsupported OS: $OS" ;;
    esac
}

# Detect CPU architecture
detect_arch() {
    ARCH="$(uname -m)"
    case "$ARCH" in
        x86_64|amd64)  echo "x86_64" ;;
        arm64|aarch64) echo "aarch64" ;;
        *) error "Unsupported architecture: $ARCH" ;;
    esac
}

# Detect libc (for Linux target selection)
detect_libc() {
    if [ "$(detect_os)" != "linux" ]; then
        return
    fi
    # Check for musl
    if ldd --version 2>&1 | grep -qi musl 2>/dev/null; then
        echo "musl"
    elif [ -f /etc/alpine-release ]; then
        echo "musl"
    else
        echo "gnu"
    fi
}

# Build the target triple
build_target() {
    OS="$(detect_os)"
    ARCH="$(detect_arch)"
    case "$OS" in
        macos)
            echo "${ARCH}-apple-darwin"
            ;;
        linux)
            LIBC="$(detect_libc)"
            echo "${ARCH}-unknown-linux-${LIBC}"
            ;;
        windows)
            echo "x86_64-pc-windows-msvc"
            ;;
    esac
}

# Get the latest release version from GitHub
get_latest_version() {
    if command -v curl > /dev/null 2>&1; then
        VERSION=$(curl -fsSL "https://api.github.com/repos/${PLUG_REPO}/releases/latest" \
            | grep '"tag_name"' \
            | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')
    elif command -v wget > /dev/null 2>&1; then
        VERSION=$(wget -qO- "https://api.github.com/repos/${PLUG_REPO}/releases/latest" \
            | grep '"tag_name"' \
            | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')
    else
        error "Neither curl nor wget found. Please install one and retry."
    fi

    if [ -z "$VERSION" ]; then
        error "Failed to determine latest version. Check your internet connection."
    fi
    echo "$VERSION"
}

# Download a file
download() {
    URL="$1"
    DEST="$2"
    info "Downloading $URL"
    if command -v curl > /dev/null 2>&1; then
        curl -fsSL --retry 3 --retry-delay 1 "$URL" -o "$DEST"
    elif command -v wget > /dev/null 2>&1; then
        wget -qO "$DEST" "$URL"
    else
        error "Neither curl nor wget found."
    fi
}

# Verify SHA-256 checksum
verify_checksum() {
    FILE="$1"
    EXPECTED="$2"
    if command -v sha256sum > /dev/null 2>&1; then
        ACTUAL=$(sha256sum "$FILE" | awk '{print $1}')
    elif command -v shasum > /dev/null 2>&1; then
        ACTUAL=$(shasum -a 256 "$FILE" | awk '{print $1}')
    else
        warn "Cannot verify checksum: sha256sum/shasum not found"
        return
    fi

    if [ "$ACTUAL" != "$EXPECTED" ]; then
        error "Checksum verification failed!\n  Expected: $EXPECTED\n  Actual:   $ACTUAL"
    fi
    success "Checksum verified"
}

# Determine install directory
choose_install_dir() {
    if [ -n "$PLUG_INSTALL_DIR" ]; then
        echo "$PLUG_INSTALL_DIR"
        return
    fi

    # Prefer ~/.local/bin if it exists or HOME is writable
    LOCAL_BIN="$HOME/.local/bin"
    if [ -d "$LOCAL_BIN" ] && [ -w "$LOCAL_BIN" ]; then
        echo "$LOCAL_BIN"
        return
    fi

    # Try creating ~/.local/bin
    if mkdir -p "$LOCAL_BIN" 2>/dev/null; then
        echo "$LOCAL_BIN"
        return
    fi

    # Fall back to /usr/local/bin (may require sudo)
    if [ -d "/usr/local/bin" ] && [ -w "/usr/local/bin" ]; then
        echo "/usr/local/bin"
        return
    fi

    # Last resort: create ~/.local/bin with user confirmation
    warn "/usr/local/bin is not writable. Installing to ~/.local/bin"
    mkdir -p "$LOCAL_BIN"
    echo "$LOCAL_BIN"
}

# Check if install dir is in PATH
check_path() {
    INSTALL_DIR="$1"
    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) ;;
        *)
            warn "${INSTALL_DIR} is not in your PATH."
            warn "Add the following to your shell profile:"
            warn "  export PATH=\"${INSTALL_DIR}:\$PATH\""
            ;;
    esac
}

main() {
    info "Installing plug — MCP multiplexer"

    TARGET="$(build_target)"
    info "Detected target: $TARGET"

    VERSION="${PLUG_VERSION:-}"
    if [ -z "$VERSION" ]; then
        info "Fetching latest release version..."
        VERSION="$(get_latest_version)"
    fi
    info "Version: $VERSION"

    # Build download URLs
    OS="$(detect_os)"
    if [ "$OS" = "windows" ]; then
        ARCHIVE_NAME="${PLUG_BIN}-${VERSION}-${TARGET}.zip"
    else
        ARCHIVE_NAME="${PLUG_BIN}-${VERSION}-${TARGET}.tar.gz"
    fi
    BASE_URL="https://github.com/${PLUG_REPO}/releases/download/${VERSION}"
    ARCHIVE_URL="${BASE_URL}/${ARCHIVE_NAME}"
    CHECKSUMS_URL="${BASE_URL}/checksums.sha256"

    # Create temp directory
    TMP_DIR="$(mktemp -d)"
    trap 'rm -rf "$TMP_DIR"' EXIT

    # Download archive and checksums
    download "$ARCHIVE_URL" "$TMP_DIR/$ARCHIVE_NAME"
    download "$CHECKSUMS_URL" "$TMP_DIR/checksums.sha256"

    # Verify checksum
    EXPECTED_CHECKSUM=$(grep "$ARCHIVE_NAME" "$TMP_DIR/checksums.sha256" | awk '{print $1}')
    if [ -n "$EXPECTED_CHECKSUM" ]; then
        verify_checksum "$TMP_DIR/$ARCHIVE_NAME" "$EXPECTED_CHECKSUM"
    else
        warn "No checksum found for $ARCHIVE_NAME — skipping verification"
    fi

    # Extract
    info "Extracting..."
    if [ "$OS" = "windows" ]; then
        unzip -q "$TMP_DIR/$ARCHIVE_NAME" -d "$TMP_DIR/extracted"
    else
        tar -xzf "$TMP_DIR/$ARCHIVE_NAME" -C "$TMP_DIR"
    fi

    # Find the binary
    if [ "$OS" = "windows" ]; then
        BIN_FILE=$(find "$TMP_DIR" -name "${PLUG_BIN}.exe" -type f | head -n1)
        BIN_DEST_NAME="${PLUG_BIN}.exe"
    else
        BIN_FILE=$(find "$TMP_DIR" -name "$PLUG_BIN" -type f | head -n1)
        BIN_DEST_NAME="$PLUG_BIN"
    fi

    if [ -z "$BIN_FILE" ]; then
        error "Binary not found in archive. Archive contents:"
    fi

    # Install
    INSTALL_DIR="$(choose_install_dir)"
    mkdir -p "$INSTALL_DIR"
    DEST="${INSTALL_DIR}/${BIN_DEST_NAME}"

    info "Installing to $DEST"
    cp "$BIN_FILE" "$DEST"
    chmod +x "$DEST"

    success "plug $VERSION installed to $DEST"

    # Check PATH
    check_path "$INSTALL_DIR"

    # Show next steps
    printf "\n"
    printf "Get started:\n"
    printf "  plug --help           Show available commands\n"
    printf "  plug connect          Connect all AI clients to all MCP servers\n"
    printf "  plug status           Check server health\n"
    printf "\n"
    printf "Documentation: https://github.com/${PLUG_REPO}\n"
}

main "$@"
