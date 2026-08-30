//! Forward proxy for testing rustup proxy authorization
//!
//! This program creates a forward proxy for testing the `RUSTUP_PROXY_AUTHORIZATION_HEADER`
//! environment variable in rustup.

use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;

use base64::Engine;
use clap::Parser;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::body::Incoming;
use hyper::header::PROXY_AUTHORIZATION;
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rustup::test::{MockDataFile, shutdown_signal};
use tokio::net::{TcpListener, TcpStream};
use tracing::{error, info, warn};

/// Forward proxy for testing rustup proxy authorization
#[derive(Parser, Debug)]
#[command(name = "rustup-mock-proxy")]
#[command(author, version, about, long_about = None)]
struct Opt {
    /// Address to bind to
    #[arg(short, long, default_value = "127.0.0.1")]
    addr: String,

    /// Port to bind to; 0 lets the OS assign a free port (the default)
    #[arg(short, long, default_value = "0")]
    port: u16,

    /// Basic auth credentials in the form "username:password"
    #[arg(long)]
    basic_test_credential: Option<String>,

    /// Where to write the data file (default:
    /// ${HOME}/.local/share/rustup-mock-proxy.data on Unix,
    /// %LOCALAPPDATA%\rustup-mock-proxy.data on Windows)
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
            .unwrap_or_else(|| MockDataFile::default_path("rustup-mock-proxy")),
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
    // bound, so its presence means the proxy is ready.
    let local_addr = listener
        .local_addr()
        .expect("bound listener has no local address");
    if let Err(e) = MockDataFile::write(
        data_file.path(),
        &local_addr.ip().to_string(),
        local_addr.port(),
        std::process::id(),
        credential.as_deref(),
        None,
    ) {
        error!("Failed to write data file {:?}: {}", data_file.path(), e);
        std::process::exit(1);
    }

    info!("Forward proxy listening on {}", local_addr);
    info!("Data file written to {:?}", data_file.path());
    info!("Forward proxy ready to accept connections");

    tokio::select! {
        _ = shutdown_signal() => info!("Shutting down"),
        _ = serve(listener, credentials) => {}
    }

    // `data_file` is dropped here, removing the data file.
}

/// Accepts connections until the process is terminated.
async fn serve(listener: TcpListener, credentials: Option<(String, String)>) {
    loop {
        let (stream, remote_addr) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to accept connection: {}", e);
                continue;
            }
        };

        info!("Proxy connection from {}", remote_addr);

        let creds = credentials.clone();
        let io = TokioIo::new(stream);

        tokio::spawn(async move {
            if let Err(e) = serve_connection(io, creds).await {
                error!("Connection error: {}", e);
            }
        });
    }
}

async fn serve_connection(
    io: TokioIo<TcpStream>,
    credentials: Option<(String, String)>,
) -> Result<(), hyper::Error> {
    let mut builder = server_http1::Builder::new();
    builder.preserve_header_case(true);
    builder.title_case_headers(true);

    builder
        .serve_connection(
            io,
            service_fn(move |req| {
                let creds = credentials.clone();
                async move { handle_proxy_request(req, creds).await }
            }),
        )
        .with_upgrades()
        .await
}

async fn handle_proxy_request(
    req: Request<Incoming>,
    credentials: Option<(String, String)>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    info!("Proxy request: {} {}", req.method(), req.uri());

    // Handle basic authentication if configured
    if let Some((username, password)) = &credentials {
        let auth_header = req.headers().get(PROXY_AUTHORIZATION);

        match auth_header {
            Some(header) => {
                let auth_str = header.to_str().unwrap_or("");
                let expected = format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD
                        .encode(format!("{}:{}", username, password).as_bytes())
                );
                if auth_str != expected {
                    info!("Authentication failed for request to {}", req.uri());
                    return Ok(Response::builder()
                        .status(StatusCode::UNAUTHORIZED)
                        .body(Full::new(Bytes::from("Unauthorized")))
                        .unwrap());
                }
            }
            None => {
                info!(
                    "Missing proxy authentication header for request to {}",
                    req.uri()
                );
                return Ok(Response::builder()
                    .status(StatusCode::PROXY_AUTHENTICATION_REQUIRED)
                    .body(Full::new(Bytes::from("Proxy requires authentication")))
                    .unwrap());
            }
        }
    }

    // Handle CONNECT requests for tunneling (used for HTTPS)
    if req.method() == Method::CONNECT {
        handle_connect_request(req).await
    } else {
        // For HTTP requests, forward directly to the target
        forward_http_request(req).await
    }
}

async fn forward_http_request(
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    // Extract the target from the URI
    let Some(target) = req.uri().authority().map(|a| a.as_str().to_string()) else {
        return Ok(Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Full::new(Bytes::from("Bad request - no authority")))
            .unwrap());
    };

    info!("Forwarding request to {}", target);

    let stream = match TcpStream::connect(&target).await {
        Ok(stream) => stream,
        Err(e) => {
            warn!("Failed to connect to {}: {}", target, e);
            return Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Full::new(Bytes::from(format!("Connection failed: {e}"))))
                .unwrap());
        }
    };

    // Forward the request through a hyper HTTP/1.1 client connection. The
    // absolute-form URI is sent unchanged, as a forward proxy requires.
    let (mut sender, conn) = match hyper::client::conn::http1::Builder::new()
        .handshake(TokioIo::new(stream))
        .await
    {
        Ok(handshake) => handshake,
        Err(e) => {
            warn!("Failed to start client connection to {}: {}", target, e);
            return Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Full::new(Bytes::from(format!("Handshake failed: {e}"))))
                .unwrap());
        }
    };
    let conn_target = target.clone();
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            warn!(
                "Client connection to {} closed with error: {}",
                conn_target, e
            );
        }
    });

    let response = match sender.send_request(req).await {
        Ok(response) => response,
        Err(e) => {
            warn!("Failed to send request to {}: {}", target, e);
            return Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Full::new(Bytes::from(format!("Send error: {e}"))))
                .unwrap());
        }
    };

    let (parts, body) = response.into_parts();
    let bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(e) => {
            warn!("Failed to read response from {}: {}", target, e);
            return Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Full::new(Bytes::new()))
                .unwrap());
        }
    };

    info!("Received response: {} bytes", bytes.len());
    Ok(Response::from_parts(parts, Full::new(bytes)))
}

async fn handle_connect_request(
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    // Extract the target address from the CONNECT request
    let target = req.uri().authority().map(|auth| auth.to_string());

    match target {
        Some(addr) => {
            info!("CONNECT tunnel request to {}", addr);

            // Upgrade the connection and trigger the tunnel
            let upgraded = hyper::upgrade::on(req).await?;

            // Handle the tunnel
            tokio::spawn(async move {
                if let Err(e) = tunnel(upgraded, &addr).await {
                    warn!("Tunnel error: {}", e);
                }
            });

            Ok(Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(Bytes::new()))
                .unwrap())
        }
        None => {
            warn!("CONNECT request without valid authority");
            let mut resp = Response::new(Full::new(Bytes::new()));
            *resp.status_mut() = StatusCode::BAD_REQUEST;
            Ok(resp)
        }
    }
}

// Create a bidirectional tunnel between the client and target server
async fn tunnel(upgraded: hyper::upgrade::Upgraded, target: &str) -> std::io::Result<()> {
    info!("Establishing tunnel to {}", target);

    // Connect to the target server
    let mut server = TcpStream::connect(target).await?;
    let mut upgraded = TokioIo::new(upgraded);

    // Proxy data between client and server using copy_bidirectional
    let (from_client, from_server) =
        tokio::io::copy_bidirectional(&mut upgraded, &mut server).await?;

    info!(
        "Tunnel complete: client wrote {} bytes, received {} bytes",
        from_client, from_server
    );

    Ok(())
}
