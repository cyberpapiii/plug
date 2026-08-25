#!/usr/bin/env sh
# plug installer
# Usage: curl -fsSL https://raw.githubusercontent.com/cyberpapiii/plug/main/install.sh | sh
# Or:    curl -fsSL https://raw.githubusercontent.com/cyberpapiii/plug/main/install.sh | sh -s -- --install-dir ~/.local/bin

set -eu

PLUG_REPO="cyberpapiii/plug"
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
error()   { printf "${RED}error${RESET} %b\n" "$*" >&2; exit 1; }

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
    curl -fsSL https://raw.githubusercontent.com/cyberpapiii/plug/main/install.sh | sh [-- OPTIONS]

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

    # Prefer ~/.local/bin when it exists and is writable
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

    # Fall back to /usr/local/bin when writable
    if [ -d "/usr/local/bin" ] && [ -w "/usr/local/bin" ]; then
        echo "/usr/local/bin"
        return
    fi

    # Last resort: create ~/.local/bin
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

restore_previous_service() {
    INSTALLED_PLUG="$1"
    PREVIOUS_PLUG="$2"
    [ -n "$PREVIOUS_PLUG" ] && [ -f "$PREVIOUS_PLUG" ] || return 1

    "$INSTALLED_PLUG" stop >/dev/null 2>&1 || true
    RESTORE_TMP="${INSTALLED_PLUG}.restore.$$"
    if ! cp "$PREVIOUS_PLUG" "$RESTORE_TMP" || ! chmod +x "$RESTORE_TMP" || ! mv "$RESTORE_TMP" "$INSTALLED_PLUG"; then
        rm -f "$RESTORE_TMP"
        return 1
    fi
    if ! "$INSTALLED_PLUG" start --output json >/dev/null 2>&1; then
        return 1
    fi
    RESTORED_STATUS=$("$INSTALLED_PLUG" status --output json 2>/dev/null || true)
    printf '%s' "$RESTORED_STATUS" | grep -Eq '"runtime_available"[[:space:]]*:[[:space:]]*true'
}

post_install_owner_setup() {
    INSTALLED_PLUG="$1"
    PREVIOUS_PLUG="${2:-}"
    if ! RESOLVED_CONFIG=$("$INSTALLED_PLUG" config resolved --output json 2>&1); then
        error "Plug could not resolve the current configuration:\n${RESOLVED_CONFIG}"
    fi
    if ! printf '%s' "$RESOLVED_CONFIG" | grep -Eq '"downstream_auth_mode"[[:space:]]*:[[:space:]]*"oauth"'; then
        return 0
    fi

    EXPECTED_VERSION=$(printf '%s' "$RESOLVED_CONFIG" | sed -n 's/.*"binary_version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)
    if [ -z "$EXPECTED_VERSION" ]; then
        error "Plug could not prove the installed binary version.\n  Run: plug start\n  Then run: plug auth owner enroll"
    fi

    WAS_RUNNING=false
    CURRENT_STATUS=$("$INSTALLED_PLUG" status --output json 2>/dev/null || true)
    if printf '%s' "$CURRENT_STATUS" | grep -Eq '"daemon_running"[[:space:]]*:[[:space:]]*true'; then
        WAS_RUNNING=true
        info "Restarting the Plug service with the newly installed binary..."
        if ! STOP_OUTPUT=$("$INSTALLED_PLUG" stop 2>&1); then
            error "Plug could not stop the previous service:\n${STOP_OUTPUT}\n  Run: plug stop\n  Then run: plug start"
        fi
    fi

    info "Starting the newly installed Plug service..."
    if ! START_OUTPUT=$("$INSTALLED_PLUG" start --output json 2>&1); then
        if [ "$WAS_RUNNING" = true ] && restore_previous_service "$INSTALLED_PLUG" "$PREVIOUS_PLUG"; then
            error "Plug could not start the configured service:\n${START_OUTPUT}\nPrevious Plug service restored and running."
        fi
        error "Plug could not start the configured service:\n${START_OUTPUT}\n  Fix the reported configuration or bind error, then run: plug start"
    fi

    if ! STATUS_JSON=$("$INSTALLED_PLUG" status --output json 2>&1); then
        if [ "$WAS_RUNNING" = true ] && restore_previous_service "$INSTALLED_PLUG" "$PREVIOUS_PLUG"; then
            error "Plug started, but service readiness could not be checked:\n${STATUS_JSON}\nPrevious Plug service restored and running."
        fi
        error "Plug started, but service readiness could not be checked:\n${STATUS_JSON}\n  Run: plug status"
    fi
    if ! printf '%s' "$STATUS_JSON" | grep -Eq '"runtime_available"[[:space:]]*:[[:space:]]*true'; then
        if [ "$WAS_RUNNING" = true ] && restore_previous_service "$INSTALLED_PLUG" "$PREVIOUS_PLUG"; then
            error "The newly installed Plug service is not ready.\n${STATUS_JSON}\nPrevious Plug service restored and running."
        fi
        error "The newly installed Plug service is not ready.\n${STATUS_JSON}\n  Run: plug status"
    fi
    if ! printf '%s' "$STATUS_JSON" | grep -Eq '"runtime_version"[[:space:]]*:[[:space:]]*"'"$EXPECTED_VERSION"'"'; then
        if [ "$WAS_RUNNING" = true ] && restore_previous_service "$INSTALLED_PLUG" "$PREVIOUS_PLUG"; then
            error "Plug is running, but it is not the newly installed version ${EXPECTED_VERSION}.\nPrevious Plug service restored and running."
        fi
        error "Plug is running, but it is not the newly installed version ${EXPECTED_VERSION}.\n  Run: plug stop\n  Then run: plug start"
    fi

    if ! OWNER_JSON=$("$INSTALLED_PLUG" auth owner list --output json 2>&1); then
        error "Downstream OAuth owner setup could not be checked:\n${OWNER_JSON}\n  Run: plug auth owner enroll"
    fi
    OWNER_JSON_COMPACT=$(printf '%s' "$OWNER_JSON" | tr -d '[:space:]')
    if [ "$OWNER_JSON_COMPACT" = "[]" ]; then
        info "Downstream OAuth needs one owner passkey. Opening setup..."
        if ! "$INSTALLED_PLUG" auth owner enroll; then
            error "Owner passkey setup could not be opened.\n  Run: plug auth owner enroll"
        fi
        info "Finish owner passkey setup in the browser before connecting a client."
    else
        success "Downstream OAuth owner passkey already enrolled"
    fi
}

main() {
    info "Installing plug — MCP multiplexer"

    OS="$(detect_os)"
    if [ "$OS" = "macos" ]; then
        error "macOS standalone CLI artifacts are not published.\nDownload and open Plug.app from the release DMG:\n  https://github.com/${PLUG_REPO}/releases\nOr install the app with Homebrew:\n  brew install --cask cyberpapiii/tap/plug-app"
    fi

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
        error "Binary not found in archive.\nArchive contents:\n$(find "$TMP_DIR" -type f | sed 's/^/  /')"
    fi

    # Install
    INSTALL_DIR="$(choose_install_dir)"
    mkdir -p "$INSTALL_DIR"
    DEST="${INSTALL_DIR}/${BIN_DEST_NAME}"

    info "Installing to $DEST"
    PREVIOUS_PLUG=""
    if [ -f "$DEST" ]; then
        PREVIOUS_PLUG="$TMP_DIR/previous-${BIN_DEST_NAME}"
        cp "$DEST" "$PREVIOUS_PLUG"
        chmod +x "$PREVIOUS_PLUG"
    fi
    cp "$BIN_FILE" "$DEST"
    chmod +x "$DEST"

    success "plug $VERSION installed to $DEST"

    # Check PATH
    check_path "$INSTALL_DIR"

    # Owner setup is required only for configurations that explicitly enable
    # downstream OAuth. Existing credentials are inspected through the local
    # authenticated operator API and are never rotated or replaced here.
    post_install_owner_setup "$DEST" "$PREVIOUS_PLUG"

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
