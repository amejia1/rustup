//! Integration tests for the `rustup-init.sh` installer script.
//!
//! `rustup-init.sh` downloads the `rustup-init` binary from a distribution
//! root and runs it, so the mock dist tree is seeded with a copy of the
//! binary built from this repo at `dist/<arch>/rustup-init`, where `<arch>`
//! is the triple the script itself detects for the host.
//!
//! Each test boots a fresh mock dist server (and proxy where relevant) on an
//! OS-assigned port, so the tests can run in parallel. The script is forced
//! to use a specific downloader (`curl` or `wget`) by running it with a
//! `PATH` that contains only the commands it needs.
//!
//! These tests only run on Unix: on Windows, `rustup-init.exe` is
//! downloaded and run directly, and the script is not used (see
//! <https://rust-lang.github.io/rustup/installation/other.html>).

#![cfg(all(feature = "test", not(windows)))]

use std::env;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::suite::proxy::{
    MockProxy, MockServer, PROXY_PASSWORD, PROXY_USER, SERVER_PASSWORD, SERVER_USER, basic_auth,
};

const RUSTUP_INIT_SH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/rustup-init.sh");

/// The commands `rustup-init.sh` needs to run: the `need_cmd()` checks in
/// `main()` (`uname`, `mktemp`, `chmod`, `mkdir`, `rm`, `rmdir`), the
/// architecture detection helpers (`head`, `tail`, `cut`, `base64`), `grep`
/// (downloader detection and error handling), and `cat` (`--help`).
const INIT_SH_COMMANDS: &[&str] = &[
    "uname", "mktemp", "chmod", "mkdir", "rm", "rmdir", "head", "tail", "cut", "base64", "grep",
    "cat",
];

/// Finds `name` in the current `PATH` (an existing, executable file).
fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for entry in env::split_paths(&path) {
        let candidate = entry.join(name);
        let Ok(metadata) = fs::metadata(&candidate) else {
            continue;
        };
        if metadata.is_file() && (metadata.permissions().mode() & 0o111) != 0 {
            return Some(candidate);
        }
    }
    None
}

/// Creates a directory of symlinks holding only the commands
/// `rustup-init.sh` needs, with `downloader` as the sole downloader, so the
/// script is forced to use it.
fn make_restricted_path(downloader: &str) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("restricted-bin-")
        .tempdir()
        .unwrap();
    for name in INIT_SH_COMMANDS.iter().chain(std::iter::once(&downloader)) {
        let target = find_in_path(name).unwrap_or_else(|| {
            panic!("{name} not found in PATH; it is required by the rustup-init.sh tests")
        });
        symlink(&target, dir.path().join(name)).unwrap();
    }
    dir
}

/// The architecture triple `rustup-init.sh` detects for the host, as the
/// script reports it through its `RUSTUP_INIT_SH_PRINT` mode.
fn detect_host_arch() -> String {
    let output = Command::new("sh")
        .arg(RUSTUP_INIT_SH)
        .env("RUSTUP_INIT_SH_PRINT", "arch")
        .output()
        .expect("failed to run rustup-init.sh in print mode");
    assert!(
        output.status.success(),
        "rustup-init.sh architecture detection failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("non-UTF-8 architecture from rustup-init.sh")
        .trim()
        .to_string()
}

/// Seeds `dist/<arch>/rustup-init` in the tree served by `server` with the
/// `rustup-init` binary built from this repo, so `rustup-init.sh` can
/// download and run it.
fn seed_rustup_init_bin(server: &MockServer) {
    let arch = detect_host_arch();
    let target = server
        .directory()
        .join("dist")
        .join(&arch)
        .join("rustup-init");
    fs::create_dir_all(target.parent().expect("dist path has a parent")).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_rustup-init"), &target).unwrap();
}

/// Starts a mock server and seeds its dist tree with the `rustup-init`
/// binary. `credentials` is in `user:password` form, or `None` for a server
/// that does not require authentication.
fn start_seeded_server(credentials: Option<&str>) -> MockServer {
    let server = MockServer::start(credentials);
    seed_rustup_init_bin(&server);
    server
}

/// Runs `rustup-init.sh -y --no-modify-path` against `dist_root` (through
/// `proxy` when given) in a fresh `RUSTUP_HOME`/`CARGO_HOME`, forcing
/// `downloader` as the script's only available downloader.
///
/// Variables that could leak in from the surrounding environment are removed
/// first, then `extra_env` is applied (the authorization header variables
/// for the authenticated scenarios).
fn run_rustup_init_sh(
    downloader: &str,
    dist_root: &str,
    proxy: Option<&str>,
    extra_env: &[(&str, &str)],
) -> (tempfile::TempDir, Output) {
    let restricted_bin = make_restricted_path(downloader);
    let home = tempfile::Builder::new()
        .prefix("rustup-home-")
        .tempdir()
        .unwrap();

    // Resolve the shell before the PATH is restricted: the kernel looks up a
    // bare command name through the child's PATH.
    let sh = find_in_path("sh")
        .expect("sh not found in PATH; it is required by the rustup-init.sh tests");
    let mut cmd = Command::new(&sh);
    cmd.arg(RUSTUP_INIT_SH);
    cmd.arg("-y");
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
    cmd.env("PATH", restricted_bin.path());
    // An empty http_proxy disables proxying.
    cmd.env("http_proxy", proxy.unwrap_or(""));
    cmd.env("https_proxy", proxy.unwrap_or(""));
    cmd.env("RUSTUP_HOME", home.path());
    cmd.env("CARGO_HOME", home.path());
    cmd.env("RUSTUP_UPDATE_ROOT", dist_root);
    cmd.env("RUSTUP_DIST_SERVER", dist_root);
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    let output = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to run rustup-init.sh");
    (home, output)
}

/// Fails the test unless `rustup-init.sh` succeeded, installed a `rustup`
/// binary into `home`, and that binary runs.
fn expect_rustup_installed(home: &Path, output: &Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let rustup_bin = home
        .join("bin")
        .join(format!("rustup{}", env::consts::EXE_SUFFIX));

    if !output.status.success() {
        panic!(
            "rustup-init.sh exited with {}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
            output.status
        );
    }
    if !rustup_bin.is_file() {
        panic!(
            "rustup-init.sh succeeded but {} was not installed\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
            rustup_bin.display()
        );
    }
    let version = Command::new(&rustup_bin)
        .arg("--version")
        .env("RUSTUP_HOME", home)
        .env("CARGO_HOME", home)
        .output()
        .expect("failed to run the installed rustup");
    if !version.status.success() {
        panic!(
            "installed rustup --version failed with {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            version.status,
            String::from_utf8_lossy(&version.stdout),
            String::from_utf8_lossy(&version.stderr),
        );
    }
}

/// `rustup-init.sh` installs rustup when only `curl` is available as a
/// downloader.
#[test]
fn rustup_init_sh_installs_with_curl() {
    let server = start_seeded_server(None);
    let (home, output) = run_rustup_init_sh("curl", &server.root_url(), None, &[]);
    expect_rustup_installed(home.path(), &output);
}

/// `rustup-init.sh` installs rustup when only `wget` is available as a
/// downloader.
#[test]
fn rustup_init_sh_installs_with_wget() {
    let server = start_seeded_server(None);
    let (home, output) = run_rustup_init_sh("wget", &server.root_url(), None, &[]);
    expect_rustup_installed(home.path(), &output);
}

/// `rustup-init.sh` installs rustup from an authenticated mock server,
/// presenting the server credentials via `RUSTUP_AUTHORIZATION_HEADER`.
#[test]
fn rustup_init_sh_installs_from_authenticated_server() {
    let credentials = format!("{SERVER_USER}:{SERVER_PASSWORD}");
    let server = start_seeded_server(Some(&credentials));
    let authorization = basic_auth(SERVER_USER, SERVER_PASSWORD);
    let (home, output) = run_rustup_init_sh(
        "curl",
        &server.root_url(),
        None,
        &[("RUSTUP_AUTHORIZATION_HEADER", authorization.as_str())],
    );
    expect_rustup_installed(home.path(), &output);
}

/// `rustup-init.sh` installs rustup through an authenticated proxy,
/// presenting both the server credentials
/// (`RUSTUP_AUTHORIZATION_HEADER`) and the proxy credentials
/// (`RUSTUP_PROXY_AUTHORIZATION_HEADER`).
#[test]
fn rustup_init_sh_installs_through_authenticated_proxy() {
    let server_credentials = format!("{SERVER_USER}:{SERVER_PASSWORD}");
    let proxy_credentials = format!("{PROXY_USER}:{PROXY_PASSWORD}");
    let server = start_seeded_server(Some(&server_credentials));
    let proxy = MockProxy::start(Some(&proxy_credentials));
    let authorization = basic_auth(SERVER_USER, SERVER_PASSWORD);
    let proxy_authorization = basic_auth(PROXY_USER, PROXY_PASSWORD);
    let (home, output) = run_rustup_init_sh(
        "wget",
        &server.root_url(),
        Some(&proxy.root_url()),
        &[
            ("RUSTUP_AUTHORIZATION_HEADER", authorization.as_str()),
            (
                "RUSTUP_PROXY_AUTHORIZATION_HEADER",
                proxy_authorization.as_str(),
            ),
        ],
    );
    expect_rustup_installed(home.path(), &output);
}
