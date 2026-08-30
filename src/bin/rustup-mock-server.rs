//! Mock distribution server for testing rustup
//!
//! This program creates a mock distribution server for testing rustup functionality.
//! It serves files over HTTP from a directory structure that mimics the rustup distribution server format.

use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use base64::Engine;
use clap::Parser;
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::body::Incoming;
use hyper::header::AUTHORIZATION;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use rustup::test::{MockDataFile, create_mock_dist_server, shutdown_signal};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tracing::{error, info, warn};

/// Mock distribution server for testing rustup
#[derive(Parser, Debug)]
#[command(name = "rustup-mock-server")]
#[command(author, version, about, long_about = None)]
struct Opt {
    /// Address to bind to
    #[arg(short, long, default_value = "127.0.0.1")]
    addr: String,

    /// Port to bind to; 0 lets the OS assign a free port (the default)
    #[arg(short, long, default_value = "0")]
    port: u16,

    /// Directory to serve (a temporary directory will be created if not specified)
    #[arg(short, long)]
    directory: Option<PathBuf>,

    /// Basic auth credentials in the form "username:password"
    #[arg(long)]
    basic_test_credential: Option<String>,

    /// Where to write the data file (default:
    /// ${HOME}/.local/share/rustup-mock-server.data on Unix,
    /// %LOCALAPPDATA%\rustup-mock-server.data on Windows)
    #[arg(long)]
    data_file: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    // Initialize logging from RUSTUP_LOG environment variable
    let log_level = env::var("RUSTUP_LOG").unwrap_or_else(|_| "info".to_string());

    // Set up tracing subscriber with the log level
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(log_level.clone()))
        .init();

    let opt = Opt::parse();

    let (directory, _temp_dir) = if let Some(dir) = opt.directory {
        (dir, None)
    } else {
        let temp = TempDir::new().expect("Failed to create temporary directory");
        (temp.path().to_path_buf(), Some(temp))
    };

    info!("Creating mock distribution server at {:?}", directory);

    // Create the mock distribution server directory structure using the test module's infrastructure
    if let Err(e) = create_mock_dist_server(&directory) {
        error!("Failed to setup mock distribution server: {}", e);
        std::process::exit(1);
    }

    info!("Mock distribution server created at {:?}", directory);

    // Parse basic auth credentials if provided
    let credentials: Option<(String, String)> = opt.basic_test_credential.as_ref().and_then(|s| {
        let parts: Vec<&str> = s.splitn(2, ':').collect();
        if parts.len() == 2 {
            Some((parts[0].to_string(), parts[1].to_string()))
        } else {
            warn!("Invalid basic auth format, expected 'username:password'");
            None
        }
    });
    let credential = credentials
        .as_ref()
        .map(|(user, pass)| format!("{user}:{pass}"));

    let data_file = MockDataFile::new(
        opt.data_file
            .clone()
            .unwrap_or_else(|| MockDataFile::default_path("rustup-mock-server")),
    );

    let addr: SocketAddr = format!("{}:{}", opt.addr, opt.port)
        .parse()
        .expect("Invalid address:port combination");

    // Create the server
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            error!("Failed to bind to {}: {}", addr, e);
            std::process::exit(1);
        }
    };

    // Record the listening address and port (and the rest of the runtime
    // configuration) in the data file. This is written after the listener is
    // bound, so its presence means the server is ready.
    let local_addr = listener
        .local_addr()
        .expect("bound listener has no local address");
    if let Err(e) = MockDataFile::write(
        data_file.path(),
        &local_addr.ip().to_string(),
        local_addr.port(),
        std::process::id(),
        credential.as_deref(),
        Some(&directory),
    ) {
        error!("Failed to write data file {:?}: {}", data_file.path(), e);
        std::process::exit(1);
    }

    info!("Mock server listening on {}", local_addr);
    info!("Data file written to {:?}", data_file.path());
    info!("Server ready to accept connections");

    let server_state = Arc::new(Mutex::new(ServerState {
        dist_dir: directory,
        credentials,
    }));

    tokio::select! {
        _ = shutdown_signal() => info!("Shutting down"),
        _ = serve(listener, server_state) => {}
    }

    // `data_file` is dropped here, removing the data file.
}

/// Accepts connections until the process is terminated.
async fn serve(listener: TcpListener, server_state: Arc<Mutex<ServerState>>) {
    loop {
        let (stream, remote_addr) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to accept connection: {}", e);
                continue;
            }
        };

        info!("Connection from {}", remote_addr);

        let server_state = server_state.clone();
        let io = hyper_util::rt::TokioIo::new(stream);

        tokio::spawn(async move {
            if let Err(e) = http1::Builder::new()
                .serve_connection(
                    io,
                    service_fn(move |req| {
                        let state = server_state.clone();
                        async move { handle_request(req, state).await }
                    }),
                )
                .await
            {
                error!("Connection error: {}", e);
            }
        });
    }
}

#[derive(Debug, Clone)]
struct ServerState {
    dist_dir: PathBuf,
    credentials: Option<(String, String)>,
}

async fn handle_request(
    req: Request<Incoming>,
    state: Arc<Mutex<ServerState>>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let state = state.lock().unwrap();

    // Handle basic authentication if configured
    if let Some((username, password)) = &state.credentials {
        if let Some(auth_header) = req.headers().get(AUTHORIZATION) {
            let auth_str = auth_header.to_str().unwrap_or("");
            let expected = format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD
                    .encode(format!("{}:{}", username, password).as_bytes())
            );
            if auth_str != expected {
                warn!("Authentication failed");
                return Ok(Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .body(Full::new(Bytes::from("Unauthorized")))
                    .unwrap());
            }
        } else {
            warn!("Missing authentication header");
            return Ok(Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .body(Full::new(Bytes::from("Authentication required")))
                .unwrap());
        }
    }

    let path = req.uri().path();
    let safe_path = path.trim_start_matches('/');

    info!("Request for: {}", path);

    let file_path = state.dist_dir.join(safe_path);

    if !file_path.exists() {
        warn!("File not found: {:?}", file_path);
        return Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::from("Not Found")))
            .unwrap());
    }

    // For simplicity, return a basic response
    let body = match std::fs::read(&file_path) {
        Ok(contents) => Full::new(Bytes::from(contents)),
        Err(e) => {
            error!("Failed to read file {:?}: {}", file_path, e);
            return Ok(Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Full::new(Bytes::from("Internal Error")))
                .unwrap());
        }
    };

    Ok(Response::new(body))
}
