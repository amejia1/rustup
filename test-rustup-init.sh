#!/bin/sh
# shellcheck shell=dash
# Test suite for the rustup-init.sh script.
#
# This script starts rustup-mock-server and rustup-mock-proxy on
# OS-assigned ports (discovered through their data files), first without
# authentication and then with basic authentication, and tests:
#   - direct and proxied access to the mock distribution server,
#   - installation with the rustup-init binary, and
#   - installation with the rustup-init.sh script, forcing the use of curl
#     or of wget by running it with a restricted PATH.
#
# Like rustup-init.sh, this script is plain POSIX sh and is expected to run
# under the /bin/sh implementations that rustup-init.sh supports
# ({a,ba,da,k,z}sh).
#
# The same scenarios are also covered by the Rust integration tests in
# tests/suite/proxy.rs (cargo test --features test).

set -eu

SERVER_USER="testuser"
SERVER_PASSWORD="testpass"
PROXY_USER="proxyuser"
PROXY_PASSWORD="proxypass"

SERVER_PID=
PROXY_PID=
TEST_RUSTUP_HOME=
DIST_DIR=
DATA_DIR=
RESTRICTED_BIN=
DIST_ROOT=
PROXY_ROOT=

# Trap to ensure cleanup on exit
cleanup() {
    echo "Cleaning up..."
    if [ -n "$SERVER_PID" ]; then
        kill "$SERVER_PID" 2>/dev/null || true
    fi
    if [ -n "$PROXY_PID" ]; then
        kill "$PROXY_PID" 2>/dev/null || true
    fi
    if [ -n "$TEST_RUSTUP_HOME" ]; then
        rm -rf "$TEST_RUSTUP_HOME" 2>/dev/null || true
    fi
    if [ -n "$DIST_DIR" ]; then
        rm -rf "$DIST_DIR" 2>/dev/null || true
    fi
    if [ -n "$DATA_DIR" ]; then
        rm -rf "$DATA_DIR" 2>/dev/null || true
    fi
    if [ -n "$DIST_DIR" ]; then
        rm -rf "$DIST_DIR" 2>/dev/null || true
    fi
    if [ -n "$RESTRICTED_BIN" ]; then
        rm -rf "$RESTRICTED_BIN" 2>/dev/null || true
    fi
}

trap cleanup EXIT

# This script needs curl for the direct mock server checks, base64 for the
# authentication tests, and mktemp for the temporary directories.
for cmd in curl base64 mktemp; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        echo "error: need '$cmd' (command not found)" >&2
        exit 1
    fi
done

# Build the test programs
echo "Building test programs..."
cargo build --features test --bin rustup-mock-server --bin rustup-mock-proxy --bin rustup-init >/dev/null 2>&1

# rustup-init.sh downloads the rustup-init binary itself from
# ${RUSTUP_UPDATE_ROOT}/dist/<arch>/rustup-init before executing it, so the
# distribution tree served by the mock server must contain a copy of the
# binary built from this repo at that path.
#
# Pre-seed a copy of the built rustup-init binary at dist/<arch>/rustup-init
# in the directory the mock server will serve. The arch triple is obtained
# from rustup-init.sh's own architecture detection (RUSTUP_INIT_SH_PRINT),
# so this works on any platform rustup-init.sh supports.
prepare_dist_dir() {
    host_arch=$(RUSTUP_INIT_SH_PRINT=arch ./rustup-init.sh)
    DIST_DIR=$(mktemp -d)
    mkdir -p "$DIST_DIR/dist/$host_arch"
    # Hard-link when possible to avoid copying the large binary; fall back
    # to a copy when on a different filesystem.
    cp -l ./target/debug/rustup-init "$DIST_DIR/dist/$host_arch/rustup-init" 2>/dev/null ||
        cp ./target/debug/rustup-init "$DIST_DIR/dist/$host_arch/rustup-init"
}
prepare_dist_dir

# Holds the mock server and proxy data files.
DATA_DIR=$(mktemp -d)

# Build a restricted PATH containing only the commands necessary to run
# rustup-init.sh, with only $1 ("curl" or "wget") available as the
# downloader, so that rustup-init.sh is forced to use it.
# Sets RESTRICTED_PATH (the PATH to use) and RESTRICTED_BIN (dir to clean up).
make_restricted_path() {
    forced_downloader=$1
    RESTRICTED_BIN=$(mktemp -d)
    RESTRICTED_PATH="$RESTRICTED_BIN"

    # The commands rustup-init.sh needs: the need_cmd() checks in main()
    # (uname, mktemp, chmod, mkdir, rm, rmdir), the architecture detection
    # helpers (head, tail, cut, base64), grep (downloader detection and
    # error handling), cat (--help), and the forced downloader.
    for cmd in uname mktemp chmod mkdir rm rmdir head tail cut base64 grep cat "$forced_downloader"; do
        if command -v "$cmd" >/dev/null 2>&1; then
            ln -s "$(command -v "$cmd")" "$RESTRICTED_BIN/$cmd"
        fi
    done
}

# Run rustup-init.sh with only one downloader available in PATH and check
# that it installed rustup. Authorization header environment variables must
# be exported by the caller as needed.
# $1: test number
# $2: test description
# $3: downloader to force ("curl" or "wget")
# $4: proxy URL (empty to run without a proxy)
rustup_init_sh_test() {
    number=$1
    description=$2
    forced=$3
    proxy=$4

    echo ""
    echo "Test ${number}: ${description}"

    make_restricted_path "$forced"

    # One directory for both RUSTUP_HOME and CARGO_HOME; the rustup shim
    # ends up in <home>/bin.
    rustup_home=$(mktemp -d)

    echo "Running: ./rustup-init.sh -y --no-modify-path (forcing ${forced} via PATH)"

    # The env prefix overrides any environment leaked from earlier tests; an
    # empty http_proxy disables proxying.
    status=0
    env PATH="$RESTRICTED_PATH" \
        http_proxy="$proxy" \
        https_proxy="$proxy" \
        RUSTUP_HOME="$rustup_home" \
        CARGO_HOME="$rustup_home" \
        RUSTUP_UPDATE_ROOT="$DIST_ROOT" \
        RUSTUP_DIST_SERVER="$DIST_ROOT" \
        ./rustup-init.sh -y --no-modify-path || status=$?

    if [ "$status" -ne 0 ]; then
        echo "Test ${number} FAILED - rustup-init.sh returned exit code ${status}"
        exit 1
    fi
    if [ -x "$rustup_home/bin/rustup" ] && \
        RUSTUP_HOME="$rustup_home" CARGO_HOME="$rustup_home" "$rustup_home/bin/rustup" --version >/dev/null 2>&1; then
        echo "Test ${number} PASSED - rustup-init.sh (${forced}) installed a working rustup"
    else
        echo "Test ${number} FAILED - rustup-init.sh ran but rustup is missing or broken"
        exit 1
    fi

    rm -rf "$rustup_home" "$RESTRICTED_BIN"
}

# Run rustup-init (the binary) against the mock server and check that it
# installed rustup.
# $1: test number
# $2: test description
# $3: proxy URL (empty to run without a proxy)
rustup_init_test() {
    number=$1
    description=$2
    proxy=$3

    echo ""
    echo "Test ${number}: ${description}"
    echo "Running: rustup-init --no-modify-path"

    # Create a temporary directory for rustup, cleaning up the previous one
    if [ -n "$TEST_RUSTUP_HOME" ]; then
        rm -rf "$TEST_RUSTUP_HOME"
    fi
    TEST_RUSTUP_HOME=$(mktemp -d)

    # The env prefix avoids leaking RUSTUP_HOME/CARGO_HOME/http_proxy into
    # later tests; an empty http_proxy disables proxying.
    #
    # -y answers every prompt, including the one Windows shows after a
    # successful install ("Press the Enter key to continue"); it is the
    # same unattended invocation the other rustup-init tests use.
    # RUSTUP_INIT_SKIP_PATH_CHECK avoids a spurious "existing Rust" prompt
    # when the machine has a non-rustup Rust on PATH (e.g. CI images ship
    # one at /rustc-sysroot/bin); see src/test/clitools.rs.
    if env http_proxy="$proxy" https_proxy="$proxy" \
        RUSTUP_HOME="$TEST_RUSTUP_HOME" CARGO_HOME="$TEST_RUSTUP_HOME" \
        RUSTUP_INIT_SKIP_PATH_CHECK=yes \
        ./target/debug/rustup-init --no-modify-path -y 2>/dev/null; then
        # Check if rustup was installed
        if [ -f "$TEST_RUSTUP_HOME/bin/rustup" ]; then
            echo "Test ${number} PASSED - rustup-init completed and set up rustup"
        else
            echo "Test ${number} FAILED - rustup-init ran but rustup program not found"
            exit 1
        fi
    else
        echo "Test ${number} FAILED - rustup-init returned non-zero exit code"
        exit 1
    fi
}

# Print the value of key $2 from the mock data file $1.
data_file_value() {
    grep "^$2=" "$1" | head -n 1 | cut -d= -f2-
}

# Wait until the data file $1 exists and holds a port. Both mock programs
# write their data file after binding their listener, so this is also the
# readiness check. Fails after 30 seconds.
wait_for_data_file() {
    file=$1
    i=0
    while [ "$i" -lt 30 ]; do
        if [ -f "$file" ] && grep -q '^port=' "$file"; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    echo "error: timed out waiting for data file $file" >&2
    return 1
}

# Start the mock server and proxy on OS-assigned ports, and record their
# URLs (DIST_ROOT and PROXY_ROOT) read from their data files.
# $1: server credentials "user:pass" (empty for no authentication)
# $2: proxy credentials "user:pass" (empty for no authentication)
start_servers() {
    SERVER_DATA_FILE="$DATA_DIR/mock-server.data"
    PROXY_DATA_FILE="$DATA_DIR/mock-proxy.data"

    if [ -n "$1" ]; then
        echo "Starting mock server with basic auth..."
        ./target/debug/rustup-mock-server --directory "$DIST_DIR" --data-file "$SERVER_DATA_FILE" --basic-test-credential "$1" >/dev/null 2>&1 &
    else
        echo "Starting mock server..."
        ./target/debug/rustup-mock-server --directory "$DIST_DIR" --data-file "$SERVER_DATA_FILE" >/dev/null 2>&1 &
    fi
    SERVER_PID=$!

    if ! wait_for_data_file "$SERVER_DATA_FILE"; then
        echo "error: mock server failed to start" >&2
        exit 1
    fi
    DIST_ROOT="http://$(data_file_value "$SERVER_DATA_FILE" addr):$(data_file_value "$SERVER_DATA_FILE" port)"

    if [ -n "$2" ]; then
        echo "Starting proxy with basic auth..."
        ./target/debug/rustup-mock-proxy --data-file "$PROXY_DATA_FILE" --basic-test-credential "$2" >/dev/null 2>&1 &
    else
        echo "Starting proxy..."
        ./target/debug/rustup-mock-proxy --data-file "$PROXY_DATA_FILE" >/dev/null 2>&1 &
    fi
    PROXY_PID=$!

    if ! wait_for_data_file "$PROXY_DATA_FILE"; then
        echo "error: mock proxy failed to start" >&2
        exit 1
    fi
    PROXY_ROOT="http://$(data_file_value "$PROXY_DATA_FILE" addr):$(data_file_value "$PROXY_DATA_FILE" port)"

    # Point rustup-init (the binary) at the mock server.
    export RUSTUP_DIST_SERVER="$DIST_ROOT"
    export RUSTUP_UPDATE_ROOT="$DIST_ROOT/rustup"
}

# Stop the mock server and proxy (they remove their own data files on exit).
stop_servers() {
    if [ -n "$SERVER_PID" ]; then
        kill "$SERVER_PID" 2>/dev/null || true
    fi
    if [ -n "$PROXY_PID" ]; then
        kill "$PROXY_PID" 2>/dev/null || true
    fi
    SERVER_PID=
    PROXY_PID=
}

# =========================================================================
# Phase 1: no authentication
# =========================================================================
start_servers "" ""

# Test 1: Direct access to mock server
echo ""
echo "Test 1: Direct access to mock server"
echo "Running: curl $DIST_ROOT/dist/channel-rust-stable.toml"
if curl -s "$DIST_ROOT/dist/channel-rust-stable.toml" | grep -q "manifest-version"; then
    echo "Test 1 PASSED"
else
    echo "Test 1 FAILED"
    exit 1
fi

# Test 2: Access through proxy
echo ""
echo "Test 2: Access through proxy"
echo "Running: curl --proxy $PROXY_ROOT $DIST_ROOT/dist/channel-rust-stable.toml"
if curl --proxy "$PROXY_ROOT" "$DIST_ROOT/dist/channel-rust-stable.toml" 2>/dev/null | grep -q "manifest-version"; then
    echo "Test 2 PASSED"
else
    echo "Test 2 FAILED"
    exit 1
fi

rustup_init_test 3 "rustup-init test" ""
rustup_init_test 4 "rustup-init test through proxy" "$PROXY_ROOT"

rustup_init_sh_test 5 "rustup-init.sh test (forcing curl)" "curl" ""
rustup_init_sh_test 6 "rustup-init.sh test (forcing wget)" "wget" ""

stop_servers

# =========================================================================
# Phase 2: basic authentication
# =========================================================================
SERVER_CREDENTIALS="${SERVER_USER}:${SERVER_PASSWORD}"
PROXY_CREDENTIALS="${PROXY_USER}:${PROXY_PASSWORD}"
start_servers "$SERVER_CREDENTIALS" "$PROXY_CREDENTIALS"

# Test 7: Access without authentication - should get 401
echo ""
echo "Test 7: Access without authentication (expect 401)"
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}\n" "$DIST_ROOT/dist/channel-rust-stable.toml")
if [ "$HTTP_CODE" = "401" ]; then
    echo "Test 7 PASSED - Got 401 Unauthorized as expected"
else
    echo "Test 7 FAILED - Got HTTP $HTTP_CODE instead of 401"
    exit 1
fi

# Test 8: Access with mock server credentials - should get 200
echo ""
echo "Test 8: Access with mock server credentials (expect 200)"
if curl --user "$SERVER_CREDENTIALS" "$DIST_ROOT/dist/channel-rust-stable.toml" 2>/dev/null | grep -q "manifest-version"; then
    echo "Test 8 PASSED - Got 200 and mock data as expected"
else
    echo "Test 8 FAILED"
    exit 1
fi

# Test 9: Access through proxy without proxy auth - should get 407
echo ""
echo "Test 9: Access through proxy without proxy auth (expect 407)"
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}\n" --proxy "$PROXY_ROOT" --user "$SERVER_CREDENTIALS" "$DIST_ROOT/dist/channel-rust-stable.toml")
if [ "$HTTP_CODE" = "407" ]; then
    echo "Test 9 PASSED - Got 407 Proxy Authentication Required as expected"
else
    echo "Test 9 FAILED - Got HTTP $HTTP_CODE instead of 407"
    exit 1
fi

# Test 10: Access through proxy with both credentials - should get 200
echo ""
echo "Test 10: Access through proxy with both credentials (expect 200)"
RESPONSE=$(curl --proxy-user "$PROXY_CREDENTIALS" --proxy "$PROXY_ROOT" --user "$SERVER_CREDENTIALS" "$DIST_ROOT/dist/channel-rust-stable.toml" 2>/dev/null)
if echo "$RESPONSE" | grep -q "manifest-version"; then
    echo "Test 10 PASSED - Got 200 and mock data as expected"
else
    echo "Test 10 FAILED"
    echo "Response: $RESPONSE"
    exit 1
fi

# Set the authorization headers for the installer tests
RUSTUP_AUTHORIZATION_HEADER="Basic $(printf '%s' "$SERVER_CREDENTIALS" | base64)"
RUSTUP_PROXY_AUTHORIZATION_HEADER="Basic $(printf '%s' "$PROXY_CREDENTIALS" | base64)"
export RUSTUP_AUTHORIZATION_HEADER RUSTUP_PROXY_AUTHORIZATION_HEADER

rustup_init_test 11 "rustup-init test with authentication" ""
rustup_init_test 12 "rustup-init test through proxy with authentication" "$PROXY_ROOT"

# Test 13 uses the server credentials only (direct download); test 14 goes
# through the proxy and needs both the server and the proxy credentials.
rustup_init_sh_test 13 "rustup-init.sh test with authentication (forcing curl)" "curl" ""
rustup_init_sh_test 14 "rustup-init.sh test through proxy with authentication (forcing wget)" "wget" "$PROXY_ROOT"

stop_servers

echo ""
echo "All tests passed!"
