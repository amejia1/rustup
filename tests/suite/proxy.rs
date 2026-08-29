//! Integration tests for `rustup-mock-server` and `rustup-mock-proxy`.
//!
//! This module is a Rust port of the `test-rustup-init.sh` shell script in
//! the repository root. The shell script is kept for manual use by
//! developers; this module runs the same scenarios as part of `cargo test
//! --features test`.
//!
//! Each test boots a fresh mock dist server (and proxy where relevant) on an
//! OS-assigned port, so the tests can run in parallel. The listening address
//! of each program is read from its data file (see `rustup::test::MockDataFile`).

#![cfg(feature = "test")]

use std::env::consts::EXE_SUFFIX;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use base64::Engine;

use rustup::test::MockDataFile;

const MOCK_SERVER: &str = env!("CARGO_BIN_EXE_rustup-mock-server");
const MOCK_PROXY: &str = env!("CARGO_BIN_EXE_rustup-mock-proxy");
const RUSTUP_INIT: &str = env!("CARGO_BIN_EXE_rustup-init");

const SERVER_USER: &str = "testuser";
const SERVER_PASSWORD: &str = "testpass";
const PROXY_USER: &str = "proxyuser";
const PROXY_PASSWORD: &str = "proxypass";
const CHANNEL_MANIFEST: &str = "dist/channel-rust-stable.toml";

/// A running `rustup-mock-server` serving a fresh mock dist tree.
///
/// The server populates its own temporary directory with the mock dist tree;
/// the test's temp directory holds the server's log and data file. The server
/// process is killed and its temp directory removed on drop.
struct MockServer {
    /// Keeps the temp dir (and the log and data file it holds) alive.
    #[allow(dead_code)]
    tmp: tempfile::TempDir,
    log: PathBuf,
    child: Child,
    addr: SocketAddr,
}

impl MockServer {
    /// Starts a server bound to an OS-assigned `127.0.0.1` port.
    ///
    /// The listening address is read from the server's data file.
    /// `credentials` is in `user:password` form, or `None` for a server that
    /// does not require authentication.
    fn start(credentials: Option<&str>) -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("mock-server-")
            .tempdir()
            .unwrap();
        let data_file = tmp.path().join("mock-server.data");
        let log = tmp.path().join("mock-server.log");
        let data_file_arg = data_file.to_str().unwrap().to_string();
        let mut child = spawn(
            MOCK_SERVER,
            &["--data-file", data_file_arg.as_str()],
            credentials,
            &log,
        );
        let addr = wait_for_data_file(&data_file, Duration::from_secs(30)).unwrap_or_else(|e| {
            kill(&mut child);
            dump_logs(&[&log]);
            panic!("mock server did not become ready: {e}");
        });

        Self {
            tmp,
            log,
            child,
            addr,
        }
    }

    /// The root URL of the server, for `RUSTUP_DIST_SERVER`.
    fn root_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The URL of `path` on the server.
    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.root_url(), path.trim_start_matches('/'))
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        kill(&mut self.child);
    }
}

/// A running `rustup-mock-proxy` bound to an OS-assigned `127.0.0.1` port.
///
/// The proxy process is killed and its temp directory removed on drop.
struct MockProxy {
    /// Keeps the temp dir (and the log and data file it holds) alive.
    #[allow(dead_code)]
    tmp: tempfile::TempDir,
    log: PathBuf,
    child: Child,
    addr: SocketAddr,
}

impl MockProxy {
    /// Starts a proxy bound to an OS-assigned `127.0.0.1` port.
    ///
    /// The listening address is read from the proxy's data file.
    /// `credentials` is in `user:password` form, or `None` to allow
    /// unauthenticated requests.
    fn start(credentials: Option<&str>) -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("mock-proxy-")
            .tempdir()
            .unwrap();
        let data_file = tmp.path().join("mock-proxy.data");
        let log = tmp.path().join("mock-proxy.log");
        let data_file_arg = data_file.to_str().unwrap().to_string();
        let mut child = spawn(
            MOCK_PROXY,
            &["--data-file", data_file_arg.as_str()],
            credentials,
            &log,
        );
        let addr = wait_for_data_file(&data_file, Duration::from_secs(30)).unwrap_or_else(|e| {
            kill(&mut child);
            dump_logs(&[&log]);
            panic!("mock proxy did not become ready: {e}");
        });

        Self {
            tmp,
            log,
            child,
            addr,
        }
    }

    /// The proxy URL, for `http_proxy`/`https_proxy`.
    fn root_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for MockProxy {
    fn drop(&mut self) {
        kill(&mut self.child);
    }
}

/// Spawns one of the mock binaries, appending its output to `log`.
fn spawn(bin: &str, args: &[&str], credentials: Option<&str>, log: &Path) -> Child {
    let stdout = File::create(log).unwrap();
    let stderr = OpenOptions::new().append(true).open(log).unwrap();

    let mut cmd = Command::new(bin);
    cmd.args(args);
    if let Some(credentials) = credentials {
        cmd.args(["--basic-test-credential", credentials]);
    }
    cmd.stdout(stdout).stderr(stderr);
    cmd.spawn().expect("failed to spawn mock binary")
}

/// Kills `child`, ignoring errors if it already exited.
fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Blocks until `path` holds a complete mock data file, returning the
/// listening address it records, or times out.
///
/// Both mock programs write the data file after binding their listener, so a
/// complete file means the service is ready.
fn wait_for_data_file(path: &Path, timeout: Duration) -> anyhow::Result<SocketAddr> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(content) = fs::read_to_string(path) {
            let data = MockDataFile::parse(&content);
            if let (Some(addr), Some(port)) = (data.get("addr"), data.get("port")) {
                return Ok(format!("{addr}:{port}").parse()?);
            }
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "data file {path:?} not ready after {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The result of a raw HTTP GET.
struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

/// Performs a minimal HTTP/1.1 GET against `url`.
///
/// If `proxy` is `Some`, the request is sent to that forward proxy in
/// absolute-URI form, like a proxying HTTP client would do. The request
/// always carries `Connection: close` and the response is read until EOF, so
/// there is no keep-alive state to manage.
fn http_get(
    url: &str,
    proxy: Option<SocketAddr>,
    headers: &[(&str, &str)],
) -> anyhow::Result<HttpResponse> {
    let (host, port, path) = parse_url(url)?;
    let (connect_addr, target) = if let Some(proxy) = proxy {
        (proxy, url.to_string())
    } else {
        let host_addr = format!("{host}:{port}").parse::<SocketAddr>()?;
        (host_addr, path)
    };

    let mut stream = TcpStream::connect(connect_addr)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;

    let mut request =
        format!("GET {target} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes())?;

    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;

    let head_end = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| anyhow::anyhow!("no end of headers in response from {connect_addr}"))?;
    let head = std::str::from_utf8(&response[..head_end])?;
    let body = response[head_end + 4..].to_vec();

    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| anyhow::anyhow!("unparseable status line in {head:?}"))?;

    Ok(HttpResponse { status, body })
}

/// Parses `http://host:port/path` into its components.
fn parse_url(url: &str) -> anyhow::Result<(String, u16, String)> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| anyhow::anyhow!("expected an http:// URL, got {url:?}"))?;
    let (host_port, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, port) = host_port
        .rsplit_once(':')
        .ok_or_else(|| anyhow::anyhow!("missing port in {url:?}"))?;
    let port = port.parse()?;
    Ok((host.to_string(), port, format!("/{path}")))
}

/// Builds a `Basic` authorization header value for `user:password`.
fn basic_auth(user: &str, password: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
    format!("Basic {encoded}")
}

/// Runs `rustup-init --no-modify-path` in a fresh `RUSTUP_HOME`/`CARGO_HOME`,
/// feeding it `1` (proceed with the standard installation) on stdin.
///
/// Variables that could leak in from the surrounding environment are removed
/// first, then `extra_env` is applied.
fn run_rustup_init(extra_env: &[(&str, &str)]) -> anyhow::Result<(tempfile::TempDir, Output)> {
    let home = tempfile::Builder::new().prefix("rustup-home-").tempdir()?;

    let mut cmd = Command::new(RUSTUP_INIT);
    cmd.arg("--no-modify-path");
    for var in [
        "http_proxy",
        "https_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "all_proxy",
        "ALL_PROXY",
        "RUSTUP_HOME",
        "CARGO_HOME",
        "RUSTUP_DIST_SERVER",
        "RUSTUP_UPDATE_ROOT",
        "RUSTUP_AUTHORIZATION_HEADER",
        "RUSTUP_PROXY_AUTHORIZATION_HEADER",
    ] {
        cmd.env_remove(var);
    }
    cmd.env("RUSTUP_HOME", home.path());
    cmd.env("CARGO_HOME", home.path());
    for (key, value) in extra_env {
        cmd.env(key, value);
    }

    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .as_mut()
        .expect("stdin is piped")
        .write_all(b"1\n")?;
    let output = child.wait_with_output()?;
    Ok((home, output))
}

/// Prints the contents of `logs` to stderr, to help diagnose failures.
fn dump_logs<P>(logs: &[P])
where
    P: AsRef<Path>,
{
    for log in logs {
        let log = log.as_ref();
        if let Ok(contents) = fs::read_to_string(log) {
            eprintln!("--- {log:?} ---\n{contents}");
        }
    }
}

/// Runs `http_get`, dumping `logs` and panicking if the request itself fails.
fn get_or_panic(
    url: &str,
    proxy: Option<SocketAddr>,
    headers: &[(&str, &str)],
    logs: &[&PathBuf],
) -> HttpResponse {
    match http_get(url, proxy, headers) {
        Ok(response) => response,
        Err(error) => {
            dump_logs(logs);
            panic!("request to {url} failed: {error}");
        }
    }
}

/// Fails the test unless the response has `status` and, when `needle` is
/// `Some`, a body containing it. Dumps `logs` on failure.
fn expect_response(response: &HttpResponse, status: u16, needle: Option<&str>, logs: &[&PathBuf]) {
    let body = String::from_utf8_lossy(&response.body);
    let matched = response.status == status && needle.is_none_or(|needle| body.contains(needle));
    if !matched {
        dump_logs(logs);
        let expected = match needle {
            Some(needle) => format!("status {status} with body containing {needle:?}"),
            None => format!("status {status}"),
        };
        panic!(
            "expected {expected}, got status {} with body: {body}",
            response.status
        );
    }
}

/// Fails the test unless `rustup-init` succeeded and installed a `rustup`
/// binary into `home`. Dumps `logs` and the captured output on failure.
fn expect_rustup_installed(home: &Path, output: &Output, logs: &[&PathBuf]) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let rustup_bin = home.join("bin").join(format!("rustup{EXE_SUFFIX}"));

    let failure = if !output.status.success() {
        Some(format!("rustup-init exited with {}", output.status))
    } else if !rustup_bin.exists() {
        Some(format!(
            "rustup-init succeeded but {} was not installed",
            rustup_bin.display()
        ))
    } else {
        None
    };

    if let Some(failure) = failure {
        dump_logs(logs);
        panic!(
            "{failure}\n--- rustup-init stdout ---\n{stdout}\n--- rustup-init stderr ---\n{stderr}"
        );
    }
}

// === test-rustup-init.sh, phase 1 (no authentication) ===

/// `test-rustup-init.sh` test 1: the mock server serves dist files directly.
#[test]
fn mock_server_serves_dist_directly() {
    let server = MockServer::start(None);
    let response = get_or_panic(&server.url(CHANNEL_MANIFEST), None, &[], &[&server.log]);
    expect_response(&response, 200, Some("manifest-version"), &[&server.log]);
}

/// `test-rustup-init.sh` test 2: the proxy forwards requests to the mock server.
#[test]
fn proxy_forwards_to_mock_server() {
    let server = MockServer::start(None);
    let proxy = MockProxy::start(None);
    let response = get_or_panic(
        &server.url(CHANNEL_MANIFEST),
        Some(proxy.addr),
        &[],
        &[&proxy.log, &server.log],
    );
    expect_response(
        &response,
        200,
        Some("manifest-version"),
        &[&proxy.log, &server.log],
    );
}

/// `test-rustup-init.sh` test 3: `rustup-init` installs a toolchain straight from
/// the mock server.
#[test]
fn rustup_init_installs_from_mock_server() {
    let server = MockServer::start(None);
    let dist_server = server.root_url();
    let update_root = format!("{dist_server}/rustup");
    let (home, output) = run_rustup_init(&[
        ("RUSTUP_DIST_SERVER", dist_server.as_str()),
        ("RUSTUP_UPDATE_ROOT", update_root.as_str()),
    ])
    .expect("failed to spawn rustup-init");
    expect_rustup_installed(home.path(), &output, &[&server.log]);
}

/// `test-rustup-init.sh` test 4: `rustup-init` installs a toolchain through the
/// proxy.
#[test]
fn rustup_init_installs_through_proxy() {
    let server = MockServer::start(None);
    let proxy = MockProxy::start(None);
    let dist_server = server.root_url();
    let update_root = format!("{dist_server}/rustup");
    let proxy_url = proxy.root_url();
    let (home, output) = run_rustup_init(&[
        ("RUSTUP_DIST_SERVER", dist_server.as_str()),
        ("RUSTUP_UPDATE_ROOT", update_root.as_str()),
        ("http_proxy", proxy_url.as_str()),
        ("https_proxy", proxy_url.as_str()),
    ])
    .expect("failed to spawn rustup-init");
    expect_rustup_installed(home.path(), &output, &[&proxy.log, &server.log]);
}

// === test-rustup-init.sh, phase 2 (basic authentication) ===

/// `test-rustup-init.sh` test 7: unauthenticated requests to the mock
/// server are rejected with 401.
#[test]
fn mock_server_rejects_unauthenticated_requests() {
    let server = MockServer::start(Some(&format!("{SERVER_USER}:{SERVER_PASSWORD}")));
    let response = get_or_panic(&server.url(CHANNEL_MANIFEST), None, &[], &[&server.log]);
    expect_response(&response, 401, None, &[&server.log]);
}

/// `test-rustup-init.sh` test 8: the mock server serves dist files to
/// clients presenting the right credentials.
#[test]
fn mock_server_serves_authenticated_requests() {
    let server = MockServer::start(Some(&format!("{SERVER_USER}:{SERVER_PASSWORD}")));
    let authorization = basic_auth(SERVER_USER, SERVER_PASSWORD);
    let response = get_or_panic(
        &server.url(CHANNEL_MANIFEST),
        None,
        &[("Authorization", &authorization)],
        &[&server.log],
    );
    expect_response(&response, 200, Some("manifest-version"), &[&server.log]);
}

/// `test-rustup-init.sh` test 9: the proxy demands its own credentials
/// even when the target's credentials are presented.
#[test]
fn proxy_rejects_requests_without_proxy_auth() {
    let server = MockServer::start(Some(&format!("{SERVER_USER}:{SERVER_PASSWORD}")));
    let proxy = MockProxy::start(Some(&format!("{PROXY_USER}:{PROXY_PASSWORD}")));
    let authorization = basic_auth(SERVER_USER, SERVER_PASSWORD);
    let response = get_or_panic(
        &server.url(CHANNEL_MANIFEST),
        Some(proxy.addr),
        &[("Authorization", &authorization)],
        &[&proxy.log, &server.log],
    );
    expect_response(&response, 407, None, &[&proxy.log, &server.log]);
}

/// `test-rustup-init.sh` test 10: with both the proxy and the target
/// authenticated, the proxy forwards the request.
#[test]
fn proxy_forwards_fully_authenticated_requests() {
    let server = MockServer::start(Some(&format!("{SERVER_USER}:{SERVER_PASSWORD}")));
    let proxy = MockProxy::start(Some(&format!("{PROXY_USER}:{PROXY_PASSWORD}")));
    let authorization = basic_auth(SERVER_USER, SERVER_PASSWORD);
    let proxy_authorization = basic_auth(PROXY_USER, PROXY_PASSWORD);
    let response = get_or_panic(
        &server.url(CHANNEL_MANIFEST),
        Some(proxy.addr),
        &[
            ("Authorization", &authorization),
            ("Proxy-Authorization", &proxy_authorization),
        ],
        &[&proxy.log, &server.log],
    );
    expect_response(
        &response,
        200,
        Some("manifest-version"),
        &[&proxy.log, &server.log],
    );
}

/// `test-rustup-init.sh` test 11: `rustup-init` installs a toolchain from
/// an authenticated mock server, presenting its credentials via
/// `RUSTUP_AUTHORIZATION_HEADER`.
#[test]
fn rustup_init_installs_from_authenticated_mock_server() {
    let server = MockServer::start(Some(&format!("{SERVER_USER}:{SERVER_PASSWORD}")));
    let dist_server = server.root_url();
    let update_root = format!("{dist_server}/rustup");
    let authorization = basic_auth(SERVER_USER, SERVER_PASSWORD);
    let (home, output) = run_rustup_init(&[
        ("RUSTUP_DIST_SERVER", dist_server.as_str()),
        ("RUSTUP_UPDATE_ROOT", update_root.as_str()),
        ("RUSTUP_AUTHORIZATION_HEADER", authorization.as_str()),
    ])
    .expect("failed to spawn rustup-init");
    expect_rustup_installed(home.path(), &output, &[&server.log]);
}

/// `test-rustup-init.sh` test 12: `rustup-init` installs a toolchain
/// through an authenticated proxy, presenting both the target credentials
/// (`RUSTUP_AUTHORIZATION_HEADER`) and the proxy credentials
/// (`RUSTUP_PROXY_AUTHORIZATION_HEADER`).
#[test]
fn rustup_init_installs_through_authenticated_proxy() {
    let server = MockServer::start(Some(&format!("{SERVER_USER}:{SERVER_PASSWORD}")));
    let proxy = MockProxy::start(Some(&format!("{PROXY_USER}:{PROXY_PASSWORD}")));
    let dist_server = server.root_url();
    let update_root = format!("{dist_server}/rustup");
    let proxy_url = proxy.root_url();
    let authorization = basic_auth(SERVER_USER, SERVER_PASSWORD);
    let proxy_authorization = basic_auth(PROXY_USER, PROXY_PASSWORD);
    let (home, output) = run_rustup_init(&[
        ("RUSTUP_DIST_SERVER", dist_server.as_str()),
        ("RUSTUP_UPDATE_ROOT", update_root.as_str()),
        ("http_proxy", proxy_url.as_str()),
        ("https_proxy", proxy_url.as_str()),
        ("RUSTUP_AUTHORIZATION_HEADER", authorization.as_str()),
        (
            "RUSTUP_PROXY_AUTHORIZATION_HEADER",
            proxy_authorization.as_str(),
        ),
    ])
    .expect("failed to spawn rustup-init");
    expect_rustup_installed(home.path(), &output, &[&proxy.log, &server.log]);
}
