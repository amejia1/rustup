# Testing

This guide explains how to test rustup features, including the mock distribution server
and proxy server for testing the `RUSTUP_AUTHORIZATION_HEADER` and
`RUSTUP_PROXY_AUTHORIZATION_HEADER` environment variables.

## Test Scripts

`test-rustup-init.sh` in the repository root is the test suite for the
`rustup-init.sh` script. It starts `rustup-mock-server` and
`rustup-mock-proxy` on OS-assigned ports (discovered through their data
files), first without authentication and then with basic authentication, and
verifies:

- that the distribution server is reachable directly and through the proxy,
- that unauthenticated requests are rejected (401 from the server, 407 from
  the proxy) and that authenticated requests succeed,
- that the `rustup-init` binary can install a toolchain in all of those cases,
  using `RUSTUP_AUTHORIZATION_HEADER` and
  `RUSTUP_PROXY_AUTHORIZATION_HEADER` where applicable, and
- that the `rustup-init.sh` script can install a toolchain, forcing the use
  of `curl` or of `wget` by running it with a restricted `PATH`.

Run it from the repository root:

```bash
./test-rustup-init.sh
```

Like `rustup-init.sh` itself, the script is plain POSIX sh and runs under the
`/bin/sh` implementations that `rustup-init.sh` supports. It builds the test
programs (with the `test` feature) if needed, uses OS-assigned localhost
ports (read from the data files the mock programs write), and cleans up its
processes and temporary directories on exit.

The same scenarios are covered by the integration tests in `tests/suite/proxy.rs`,
which run the mock server and proxy on OS-assigned ports, discovered through
their data files, so they can run in parallel:

```bash
cargo test --features test --test test_bonanza proxy::
```

## Building the Test Programs

All test-related programs require the `test` feature to be enabled. Build them as follows:

```bash
# Build all test programs
cargo build --features test

# Build individual test binaries
cargo build --features test --bin rustup-mock-server
cargo build --features test --bin rustup-mock-proxy
```

## Data Files

Both `rustup-mock-server` and `rustup-mock-proxy` write a data file once
their listener is bound, and remove it when they exit (after a normal exit or
SIGINT/SIGTERM). The presence of the file is therefore a readiness signal:
tests wait for it before talking to the program, and read it to learn the
address and port the program is listening on (important when the port is
OS-assigned, which is the default).

The file contains one `key=value` pair per line:

| Key          | Description                                              |
| ------------ | -------------------------------------------------------- |
| `addr`       | The address the program is listening on                  |
| `port`       | The port the program is listening on                     |
| `pid`        | The process id of the program                            |
| `credential` | The basic test credential in use (only when `--basic-test-credential` was given) |
| `directory`  | The directory being served (`rustup-mock-server` only)   |

The location of the data file is set with the `--data-file` option. When it
is not given, the default locations are
`${HOME}/.local/share/rustup-mock-server.data` and
`${HOME}/.local/share/rustup-mock-proxy.data` on Unix, and
`%LOCALAPPDATA%\rustup-mock-server.data` and
`%LOCALAPPDATA%\rustup-mock-proxy.data` on Windows.

A shell one-liner to read the port from a data file:

```bash
PORT=$(grep '^port=' "${HOME}/.local/share/rustup-mock-server.data" | cut -d= -f2)
```

## Mock Distribution Server (`rustup-mock-server`)

The `rustup-mock-server` program creates a mock distribution server for testing rustup functionality. It serves files over HTTP from a directory structure that mimics the rustup distribution server format.

### Running the Mock Server

Basic usage:

```bash
# Start the mock server (it picks a free port and records it in its data
# file, ${HOME}/.local/share/rustup-mock-server.data by default)
./target/debug/rustup-mock-server

# Use a specific port
./target/debug/rustup-mock-server --port 8080

# Bind to a specific address
./target/debug/rustup-mock-server --addr 0.0.0.0

# Specify a directory to serve from
./target/debug/rustup-mock-server --directory /path/to/mock/dist

# Use Basic authentication
./target/debug/rustup-mock-server --basic-test-credential "testuser:testpass"

# Write the data file somewhere else
./target/debug/rustup-mock-server --data-file /tmp/my-mock-server.data
```

### Command Line Options

| Option | Description |
|--------|-------------|
| `--addr <ADDR>` | Address to bind to (default: "127.0.0.1") |
| `--port <PORT>` | Port to bind to; 0 lets the OS assign a free port (default: 0) |
| `--directory <DIR>` | Directory to serve files from. If not specified, a temporary directory will be created |
| `--basic-test-credential <CREDENTIALS>` | Basic auth credentials in the form "username:password" |
| `--data-file <PATH>` | Where to write the data file (default: ${HOME}/.local/share/rustup-mock-server.data, or %LOCALAPPDATA%\rustup-mock-server.data on Windows) |

### Environment Variables

The server uses the `RUSTUP_LOG` environment variable for logging configuration:

```bash
# Enable debug logging
RUSTUP_LOG="debug" ./target/debug/rustup-mock-server
```

### Using with rustup

To test rustup against the mock server:

```bash
# Create a directory for rustup
export RUSTUP_HOME="$(mktemp -d)"
export CARGO_HOME="${RUSTUP_HOME}"

# Start the mock server in the background
./target/debug/rustup-mock-server &
MOCK_PID=$!

# Wait for the server to start (its data file appears once it is listening)
DATA_FILE="${HOME}/.local/share/rustup-mock-server.data"
while ! grep -q '^port=' "$DATA_FILE" 2>/dev/null; do sleep 1; done
PORT=$(grep '^port=' "$DATA_FILE" | cut -d= -f2)

# Point rustup at the mock server
export RUSTUP_DIST_SERVER="http://127.0.0.1:${PORT}"
export RUSTUP_UPDATE_ROOT="${RUSTUP_DIST_SERVER}/rustup"

# Initialize RUSTUP_HOME directory
./target/debug/rustup-init --no-modify-path

# Run rustup commands
./target/debug/rustup --default stable

# Clean up (the server removes its data file on exit)
kill $MOCK_PID
rm -rf "$RUSTUP_HOME"
```

## Forward Proxy (`rustup-mock-proxy`)

The `rustup-mock-proxy` program creates a forward proxy for testing the `RUSTUP_PROXY_AUTHORIZATION_HEADER` environment variable.

### Running the Proxy

Basic usage:

```bash
# Start the proxy (it picks a free port and records it in its data file,
# ${HOME}/.local/share/rustup-mock-proxy.data by default)
./target/debug/rustup-mock-proxy

# Use a specific port
./target/debug/rustup-mock-proxy --port 8081

# Bind to a different address
./target/debug/rustup-mock-proxy --addr 0.0.0.0

# Use Basic authentication
./target/debug/rustup-mock-proxy --basic-test-credential "proxyuser:proxypass"

# Write the data file somewhere else
./target/debug/rustup-mock-proxy --data-file /tmp/my-mock-proxy.data
```

### Command Line Options

| Option | Description |
|--------|-------------|
| `--addr <ADDR>` | Address to bind to (default: "127.0.0.1") |
| `--port <PORT>` | Port to bind to; 0 lets the OS assign a free port (default: 0) |
| `--basic-test-credential <CREDENTIALS>` | Basic auth credentials in the form "username:password" for the `Proxy-Authorization` header |
| `--data-file <PATH>` | Where to write the data file (default: ${HOME}/.local/share/rustup-mock-proxy.data, or %LOCALAPPDATA%\rustup-mock-proxy.data on Windows) |

### Using with rustup

To test rustup with the proxy, run the mock server as the distribution server and
route rustup's downloads through the proxy:

```bash
# Create a directory for rustup
export RUSTUP_HOME="$(mktemp -d)"
export CARGO_HOME="${RUSTUP_HOME}"

# Start the mock server and the proxy in the background
./target/debug/rustup-mock-server &
MOCK_PID=$!
./target/debug/rustup-mock-proxy --basic-test-credential "proxyuser:proxypass" &
PROXY_PID=$!

SERVER_DATA_FILE="${HOME}/.local/share/rustup-mock-server.data"
PROXY_DATA_FILE="${HOME}/.local/share/rustup-mock-proxy.data"

# Wait for the programs to start (their data files appear once they are
# listening)
while ! grep -q '^port=' "$SERVER_DATA_FILE" 2>/dev/null; do sleep 1; done
while ! grep -q '^port=' "$PROXY_DATA_FILE" 2>/dev/null; do sleep 1; done
SERVER_PORT=$(grep '^port=' "$SERVER_DATA_FILE" | cut -d= -f2)
PROXY_PORT=$(grep '^port=' "$PROXY_DATA_FILE" | cut -d= -f2)

# Set the proxy credentials
export RUSTUP_PROXY_AUTHORIZATION_HEADER="Basic $(echo -n 'proxyuser:proxypass' | base64)"

# Point rustup at the mock server through the proxy
export RUSTUP_DIST_SERVER="http://127.0.0.1:${SERVER_PORT}"
export RUSTUP_UPDATE_ROOT="${RUSTUP_DIST_SERVER}/rustup"
export http_proxy="http://127.0.0.1:${PROXY_PORT}"
export https_proxy="http://127.0.0.1:${PROXY_PORT}"

# Initialize RUSTUP_HOME directory
./target/debug/rustup-init --no-modify-path

# Run rustup commands through the proxy
./target/debug/rustup --default stable

# Clean up (the programs remove their data files on exit)
kill $MOCK_PID $PROXY_PID
rm -rf "$RUSTUP_HOME"
```

## Testing Authorization Headers

The `RUSTUP_AUTHORIZATION_HEADER` and `RUSTUP_PROXY_AUTHORIZATION_HEADER` environment variables allow you to set HTTP headers for downloads.

### Testing with the Mock Server

```bash
# Create a directory for rustup
export RUSTUP_HOME="$(mktemp -d)"
export CARGO_HOME="${RUSTUP_HOME}"

# Start the mock server with basic auth
./target/debug/rustup-mock-server --basic-test-credential "testuser:testpass" &
MOCK_PID=$!

# Wait for the server to start (its data file appears once it is listening)
DATA_FILE="${HOME}/.local/share/rustup-mock-server.data"
while ! grep -q '^port=' "$DATA_FILE" 2>/dev/null; do sleep 1; done
PORT=$(grep '^port=' "$DATA_FILE" | cut -d= -f2)

# Set the authorization header
export RUSTUP_AUTHORIZATION_HEADER="Basic $(echo -n 'testuser:testpass' | base64)"

# Point rustup at the mock server
export RUSTUP_DIST_SERVER="http://127.0.0.1:${PORT}"
export RUSTUP_UPDATE_ROOT="${RUSTUP_DIST_SERVER}/rustup"

# Initialize RUSTUP_HOME directory
./target/debug/rustup-init --no-modify-path

# Run rustup commands that require authentication
./target/debug/rustup --default stable

# Clean up (the server removes its data file on exit)
kill $MOCK_PID
rm -rf "$RUSTUP_HOME"
```

### Testing with the Proxy

To test the `RUSTUP_PROXY_AUTHORIZATION_HEADER` against a proxy that requires
authentication, run both the mock server and the proxy with credentials:

```bash
# Create a directory for rustup
export RUSTUP_HOME="$(mktemp -d)"
export CARGO_HOME="${RUSTUP_HOME}"

# Start the mock server and the proxy with basic auth
./target/debug/rustup-mock-server --basic-test-credential "testuser:testpass" &
MOCK_PID=$!
./target/debug/rustup-mock-proxy --basic-test-credential "proxyuser:proxypass" &
PROXY_PID=$!

SERVER_DATA_FILE="${HOME}/.local/share/rustup-mock-server.data"
PROXY_DATA_FILE="${HOME}/.local/share/rustup-mock-proxy.data"

# Wait for the programs to start (their data files appear once they are
# listening)
while ! grep -q '^port=' "$SERVER_DATA_FILE" 2>/dev/null; do sleep 1; done
while ! grep -q '^port=' "$PROXY_DATA_FILE" 2>/dev/null; do sleep 1; done
SERVER_PORT=$(grep '^port=' "$SERVER_DATA_FILE" | cut -d= -f2)
PROXY_PORT=$(grep '^port=' "$PROXY_DATA_FILE" | cut -d= -f2)

# Set the authorization headers
export RUSTUP_AUTHORIZATION_HEADER="Basic $(echo -n 'testuser:testpass' | base64)"
export RUSTUP_PROXY_AUTHORIZATION_HEADER="Basic $(echo -n 'proxyuser:proxypass' | base64)"

# Point rustup at the mock server through the proxy
export RUSTUP_DIST_SERVER="http://127.0.0.1:${SERVER_PORT}"
export RUSTUP_UPDATE_ROOT="${RUSTUP_DIST_SERVER}/rustup"
export http_proxy="http://127.0.0.1:${PROXY_PORT}"
export https_proxy="http://127.0.0.1:${PROXY_PORT}"

# Initialize RUSTUP_HOME directory
./target/debug/rustup-init --no-modify-path

# Run rustup commands through the proxy
./target/debug/rustup --default stable

# Clean up (the programs remove their data files on exit)
kill $MOCK_PID $PROXY_PID
rm -rf "$RUSTUP_HOME"
```

## Unit and Integration Tests

Unit tests for the authorization headers are located in `src/download/tests.rs`. These
tests verify the HTTP header functionality with a real HTTP server using the `hyper`
server infrastructure.

Integration tests for the mock server and proxy are located in
`tests/suite/proxy.rs`. They cover the same scenarios as the test scripts and run with
`cargo test --features test`. Each test starts the mock programs with
`--data-file` pointing into a test-specific temporary directory, and reads the
listening address from the data file (see the "Data Files" section above).
