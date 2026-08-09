#!/usr/bin/env sh
set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
TEST_DIR=$(mktemp -d /tmp/plug-task5.XXXXXX)
REAL_PLUG=${PLUG_TEST_BIN:-"$ROOT_DIR/target/debug/plug"}
CALLS="$TEST_DIR/calls"
WRAPPER="$TEST_DIR/plug"
BIND_PID=""
TEST_HOMES=""

cleanup() {
    for test_home in $TEST_HOMES; do
        plug_env "$test_home" "$REAL_PLUG" stop >/dev/null 2>&1 || true
    done
    if [ -n "$BIND_PID" ]; then
        kill "$BIND_PID" >/dev/null 2>&1 || true
        wait "$BIND_PID" 2>/dev/null || true
    fi
    rm -rf "$TEST_DIR"
}
trap cleanup EXIT INT TERM

[ -x "$REAL_PLUG" ] || {
    printf 'real Plug test binary not found: %s\n' "$REAL_PLUG" >&2
    exit 1
}

plug_env() {
    test_home=$1
    shift
    env HOME="$test_home" \
        XDG_CONFIG_HOME="$test_home/config" \
        XDG_RUNTIME_DIR="$test_home/runtime" \
        XDG_STATE_HOME="$test_home/state" \
        PATH="$PATH" \
        "$@"
}

free_port() {
    python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()'
}

write_oauth_config() {
    test_home=$1
    port=$2
    config_path=$(plug_env "$test_home" "$REAL_PLUG" config --path)
    mkdir -p "$(dirname "$config_path")"
    {
        printf '[http]\n'
        printf 'bind_address = "127.0.0.1"\n'
        printf 'port = %s\n' "$port"
        printf 'auth_mode = "oauth"\n'
        printf 'public_base_url = "https://plug.example.com"\n'
        printf 'oauth_scopes = ["tools:read"]\n'
    } > "$config_path"
    printf '%s\n' "$config_path"
}

daemon_pid() {
    test_home=$1
    pid_file=$(find "$test_home" -name plug.pid -type f -print -quit)
    [ -n "$pid_file" ]
    tr -d '[:space:]' < "$pid_file"
}

assert_called() {
    expected=$1
    grep -Fxq "$expected" "$CALLS" || {
        printf 'expected call not found: %s\n' "$expected" >&2
        exit 1
    }
}

assert_not_called() {
    unexpected=$1
    if grep -Fxq "$unexpected" "$CALLS"; then
        printf 'unexpected call found: %s\n' "$unexpected" >&2
        exit 1
    fi
}

assert_resolved_oauth() {
    fixture=$1
    shift
    output=$(env HOME="$TEST_DIR/config-home" PATH="$PATH" "$@" \
        "$REAL_PLUG" --config "$fixture" --output json config resolved)
    printf '%s' "$output" | grep -Eq '"downstream_auth_mode"[[:space:]]*:[[:space:]]*"oauth"'
    printf '%s' "$output" | grep -Eq '"binary_version"[[:space:]]*:[[:space:]]*"[^"]+"'
}

mkdir -p "$TEST_DIR/config-home"
EMPTY_CONFIG="$TEST_DIR/empty.toml"
: > "$EMPTY_CONFIG"
assert_resolved_oauth "$EMPTY_CONFIG" \
    PLUG_HTTP__AUTH_MODE=oauth \
    PLUG_HTTP__PUBLIC_BASE_URL=https://plug.example.com \
    'PLUG_HTTP__OAUTH_SCOPES=["tools:read"]'

DOTTED_CONFIG="$TEST_DIR/dotted.toml"
cat > "$DOTTED_CONFIG" <<'EOF'
http.auth_mode = "oauth"
http.public_base_url = "https://plug.example.com"
http.oauth_scopes = ["tools:read"]
EOF
assert_resolved_oauth "$DOTTED_CONFIG"

QUOTED_CONFIG="$TEST_DIR/quoted.toml"
cat > "$QUOTED_CONFIG" <<'EOF'
["http"]
auth_mode = "oauth"
public_base_url = "https://plug.example.com"
oauth_scopes = ["tools:read"]
EOF
assert_resolved_oauth "$QUOTED_CONFIG"

ORDINARY_CONFIG="$TEST_DIR/ordinary.toml"
cat > "$ORDINARY_CONFIG" <<'EOF'
[http]
auth_mode = "oauth"
public_base_url = "https://plug.example.com"
oauth_scopes = ["tools:read"]
EOF
assert_resolved_oauth "$ORDINARY_CONFIG"

# Load installer functions without downloading or installing a release.
sed 's/^main "\$@"$/:/' "$ROOT_DIR/install.sh" > "$TEST_DIR/install-lib.sh"
# shellcheck source=/dev/null
. "$TEST_DIR/install-lib.sh"

export REAL_PLUG CALLS
cat > "$WRAPPER" <<'EOF'
#!/usr/bin/env sh
printf '%s\n' "$*" >> "$CALLS"
case "$*" in
    "auth owner enroll")
        exit 0
        ;;
    *)
        exec "$REAL_PLUG" "$@"
        ;;
esac
EOF
chmod +x "$WRAPPER"

# A fresh OAuth install must resolve through Plug, start the installed binary,
# prove the live runtime version, inspect the real owner API, and open setup.
FRESH_HOME="$TEST_DIR/fresh-home"
TEST_HOMES="$TEST_HOMES $FRESH_HOME"
mkdir -p "$FRESH_HOME"
write_oauth_config "$FRESH_HOME" "$(free_port)" >/dev/null
: > "$CALLS"
(
    export HOME="$FRESH_HOME"
    export XDG_CONFIG_HOME="$FRESH_HOME/config"
    export XDG_RUNTIME_DIR="$FRESH_HOME/runtime"
    export XDG_STATE_HOME="$FRESH_HOME/state"
    post_install_owner_setup "$WRAPPER"
)
assert_called "config resolved --output json"
assert_called "start --output json"
assert_called "status --output json"
assert_called "auth owner list --output json"
assert_called "auth owner enroll"
fresh_status=$(plug_env "$FRESH_HOME" "$REAL_PLUG" status --output json)
printf '%s' "$fresh_status" | grep -Eq '"runtime_available"[[:space:]]*:[[:space:]]*true'
printf '%s' "$fresh_status" | grep -Eq '"runtime_version"[[:space:]]*:[[:space:]]*"[^"]+"'

# An upgrade must replace an already-running daemon before trusting owner APIs.
STALE_HOME="$TEST_DIR/stale-home"
TEST_HOMES="$TEST_HOMES $STALE_HOME"
mkdir -p "$STALE_HOME"
write_oauth_config "$STALE_HOME" "$(free_port)" >/dev/null
plug_env "$STALE_HOME" "$REAL_PLUG" start --output json >/dev/null
stale_pid=$(daemon_pid "$STALE_HOME")
: > "$CALLS"
(
    export HOME="$STALE_HOME"
    export XDG_CONFIG_HOME="$STALE_HOME/config"
    export XDG_RUNTIME_DIR="$STALE_HOME/runtime"
    export XDG_STATE_HOME="$STALE_HOME/state"
    post_install_owner_setup "$WRAPPER"
)
replacement_pid=$(daemon_pid "$STALE_HOME")
[ "$replacement_pid" != "$stale_pid" ] || {
    printf 'installer trusted the stale daemon process %s\n' "$stale_pid" >&2
    exit 1
}
assert_called "stop"
assert_called "start --output json"
assert_called "auth owner list --output json"

# Startup failures must retain the exact bind cause from the new daemon.
BIND_HOME="$TEST_DIR/bind-home"
TEST_HOMES="$TEST_HOMES $BIND_HOME"
mkdir -p "$BIND_HOME"
BIND_PORT=$(free_port)
BIND_READY="$TEST_DIR/bind-ready"
python3 - "$BIND_PORT" "$BIND_READY" <<'PY' &
import pathlib
import socket
import sys
import time

listener = socket.socket()
listener.bind(("127.0.0.1", int(sys.argv[1])))
listener.listen()
pathlib.Path(sys.argv[2]).touch()
time.sleep(60)
PY
BIND_PID=$!
while [ ! -f "$BIND_READY" ]; do
    kill -0 "$BIND_PID"
    sleep 0.05
done
write_oauth_config "$BIND_HOME" "$BIND_PORT" >/dev/null
if bind_output=$(
    export HOME="$BIND_HOME"
    export XDG_CONFIG_HOME="$BIND_HOME/config"
    export XDG_RUNTIME_DIR="$BIND_HOME/runtime"
    export XDG_STATE_HOME="$BIND_HOME/state"
    post_install_owner_setup "$WRAPPER" 2>&1
); then
    printf 'installer unexpectedly accepted an occupied OAuth bind\n' >&2
    exit 1
fi
printf '%s' "$bind_output" | grep -Fq "failed to bind downstream HTTP address 127.0.0.1:$BIND_PORT"

# Invalid resolved OAuth configuration must retain Plug's exact public URL cause.
URL_HOME="$TEST_DIR/url-home"
mkdir -p "$URL_HOME"
url_config=$(write_oauth_config "$URL_HOME" "$(free_port)")
sed 's#https://plug.example.com#http://127.0.0.1:3282#' "$url_config" > "$TEST_DIR/url-invalid.toml"
mv "$TEST_DIR/url-invalid.toml" "$url_config"
if url_output=$(
    export HOME="$URL_HOME"
    export XDG_CONFIG_HOME="$URL_HOME/config"
    export XDG_RUNTIME_DIR="$URL_HOME/runtime"
    export XDG_STATE_HOME="$URL_HOME/state"
    post_install_owner_setup "$WRAPPER" 2>&1
); then
    printf 'installer unexpectedly accepted an invalid OAuth public URL\n' >&2
    exit 1
fi
printf '%s' "$url_output" | grep -Fq 'http.public_base_url must be an HTTPS origin with a domain hostname'

# Non-OAuth configuration remains a no-op and never starts a daemon.
AUTO_HOME="$TEST_DIR/auto-home"
mkdir -p "$AUTO_HOME"
auto_config=$(plug_env "$AUTO_HOME" "$REAL_PLUG" config --path)
mkdir -p "$(dirname "$auto_config")"
printf '[http]\nauth_mode = "auto"\n' > "$auto_config"
: > "$CALLS"
(
    export HOME="$AUTO_HOME"
    export XDG_CONFIG_HOME="$AUTO_HOME/config"
    export XDG_RUNTIME_DIR="$AUTO_HOME/runtime"
    export XDG_STATE_HOME="$AUTO_HOME/state"
    post_install_owner_setup "$WRAPPER"
)
assert_called "config resolved --output json"
assert_not_called "start --output json"
assert_not_called "auth owner list --output json"
