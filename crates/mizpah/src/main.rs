mod api;
mod attach;
mod filter;
mod ingest;
mod pretty_ingest;
mod store;

use api::AppState;
use clap::Parser;
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use store::{Store, DEFAULT_MAX_BYTES};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "mizpah",
    about = "JSON log viewer — pipe logs and inspect them in a web UI",
    version
)]
struct Cli {
    /// Service name for this ingest stream (required)
    #[arg(short, long)]
    service: String,

    /// Host to bind (hub) or connect to (attach)
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Port to bind (hub) or connect to (attach)
    #[arg(short, long, default_value_t = 1738)]
    port: u16,

    /// Max in-memory log bytes (hub only)
    #[arg(long, default_value_t = DEFAULT_MAX_BYTES)]
    max_bytes: u64,

    /// Do not open the browser when starting as hub
    #[arg(long, default_value_t = false)]
    no_open: bool,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("error")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    if cli.service.trim().is_empty() {
        eprintln!("error: --service must not be empty");
        std::process::exit(2);
    }

    let addr: SocketAddr = format!("{}:{}", cli.host, cli.port)
        .parse()
        .unwrap_or_else(|e| {
            eprintln!("error: invalid host/port: {e}");
            std::process::exit(2);
        });

    match try_bind(addr) {
        Ok(listener) => {
            if let Err(err) = run_hub(listener, cli).await {
                error!(error = %err, "hub failed");
                std::process::exit(1);
            }
        }
        Err(bind_err) => {
            // Address in use → attach mode
            let in_use = bind_err.kind() == std::io::ErrorKind::AddrInUse;
            if !in_use {
                eprintln!(
                    "error: could not bind {addr}: {bind_err}\n\
                     hint: if another Mizpah hub should be used, ensure it is reachable"
                );
                std::process::exit(1);
            }

            let base_url = format!("http://{}:{}", cli.host, cli.port);
            info!(%addr, "port in use; attaching as ingest client");
            if let Err(err) = attach::attach_and_forward(&base_url, &cli.service).await {
                eprintln!("error: {err}");
                std::process::exit(1);
            }
        }
    }
}

fn try_bind(addr: SocketAddr) -> std::io::Result<TcpListener> {
    TcpListener::bind(addr)
}

fn print_startup_banner(url: &str) {
    eprintln!(
        r#" __  __ ___ ________  ___   _  _
|  \/  |_ _|_  /  _ \/ _ \ | || |
| |\/| || | / /| |_) / _` || __ |
| |  | || |/ /_|  __/ (_| || ||_|
|_|  |_|___/____|_|   \__,_| \__/

{url}"#
    );
}

async fn run_hub(
    std_listener: TcpListener,
    cli: Cli,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    std_listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(std_listener)?;

    let store = Arc::new(Store::new(cli.max_bytes));
    let state = AppState {
        store: Arc::clone(&store),
    };
    let app = api::router(state);

    let url = format!("http://{}:{}", cli.host, cli.port);
    print_startup_banner(&url);

    if !cli.no_open {
        if let Err(err) = open::that(&url) {
            tracing::warn!(error = %err, "failed to open browser");
        }
    }

    let service = cli.service.clone();
    let ingest_store = Arc::clone(&store);
    tokio::spawn(async move {
        ingest::ingest_stdin_local(ingest_store, service).await;
    });

    axum::serve(listener, app).await?;
    Ok(())
}
