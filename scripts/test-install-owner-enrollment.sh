#!/usr/bin/env sh
set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
TEST_DIR=$(mktemp -d)
trap 'rm -rf "$TEST_DIR"' EXIT

# Load installer functions without downloading or installing a release.
sed 's/^main "\$@"$/:/' "$ROOT_DIR/install.sh" > "$TEST_DIR/install-lib.sh"
# shellcheck source=/dev/null
. "$TEST_DIR/install-lib.sh"

FAKE_CONFIG="$TEST_DIR/config.toml"
FAKE_CALLS="$TEST_DIR/calls"
FAKE_PLUG="$TEST_DIR/plug"
export FAKE_CONFIG FAKE_CALLS

cat > "$FAKE_PLUG" <<'EOF'
#!/usr/bin/env sh
printf '%s\n' "$*" >> "$FAKE_CALLS"
case "$*" in
    "config --path")
        printf '%s\n' "$FAKE_CONFIG"
        ;;
    "status --output json")
        if [ "${FAKE_RUNTIME_READY:-1}" = "1" ]; then
            printf '%s\n' '{"runtime_available":true}'
        else
            printf '%s\n' '{"runtime_available":false}'
        fi
        ;;
    "auth owner list --output json")
        if [ "${FAKE_LIST_FAIL:-0}" = "1" ]; then
            exit 1
        fi
        printf '%s\n' "${FAKE_OWNER_LIST:-[]}"
        ;;
    "auth owner enroll")
        [ "${FAKE_ENROLL_FAIL:-0}" != "1" ]
        ;;
    *)
        printf 'unexpected fake plug invocation: %s\n' "$*" >&2
        exit 90
        ;;
esac
EOF
chmod +x "$FAKE_PLUG"

assert_called() {
    expected=$1
    grep -Fxq "$expected" "$FAKE_CALLS" || {
        printf 'expected call not found: %s\n' "$expected" >&2
        exit 1
    }
}

assert_not_called() {
    unexpected=$1
    if grep -Fxq "$unexpected" "$FAKE_CALLS"; then
        printf 'unexpected call found: %s\n' "$unexpected" >&2
        exit 1
    fi
}

cat > "$FAKE_CONFIG" <<'EOF'
[http]
auth_mode = "auto"
EOF
: > "$FAKE_CALLS"
post_install_owner_setup "$FAKE_PLUG"
assert_called "config --path"
assert_not_called "auth owner list --output json"

cat > "$FAKE_CONFIG" <<'EOF'
[http]
auth_mode = "oauth"
public_base_url = "https://plug.example.com"
EOF
: > "$FAKE_CALLS"
FAKE_OWNER_LIST='[]' post_install_owner_setup "$FAKE_PLUG"
assert_called "status --output json"
assert_called "auth owner list --output json"
assert_called "auth owner enroll"

: > "$FAKE_CALLS"
FAKE_OWNER_LIST='[{"credential_id":"public-summary-only"}]' post_install_owner_setup "$FAKE_PLUG"
assert_not_called "auth owner enroll"

: > "$FAKE_CALLS"
if output=$(FAKE_RUNTIME_READY=0 post_install_owner_setup "$FAKE_PLUG" 2>&1); then
    printf 'OAuth install readiness unexpectedly succeeded without runtime proof\n' >&2
    exit 1
fi
printf '%s' "$output" | grep -Fq 'plug start'
printf '%s' "$output" | grep -Fq 'plug auth owner enroll'

: > "$FAKE_CALLS"
if output=$(FAKE_LIST_FAIL=1 post_install_owner_setup "$FAKE_PLUG" 2>&1); then
    printf 'OAuth install readiness unexpectedly succeeded when owner inspection failed\n' >&2
    exit 1
fi
printf '%s' "$output" | grep -Fq 'plug auth owner enroll'
