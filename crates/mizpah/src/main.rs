mod api;
mod attach;
mod filter;
mod ingest;
mod mcp;
mod models;
mod pretty_ingest;
mod properties;
mod stdin_lines;
mod store;

use api::AppState;
use clap::{Parser, Subcommand};
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
    #[command(subcommand)]
    command: Option<Commands>,

    /// Service name for this ingest stream (required in pipe/hub mode)
    #[arg(short, long, global = false)]
    service: Option<String>,

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

#[derive(Debug, Subcommand)]
enum Commands {
    /// MCP server for Cursor / Claude / Codex (queries the live hub)
    Mcp {
        #[command(subcommand)]
        action: Option<McpAction>,

        /// Hub host (ignored when MIZPAH_URL is set)
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// Hub port (ignored when MIZPAH_URL is set)
        #[arg(short, long, default_value_t = 1738)]
        port: u16,
    },
}

#[derive(Debug, Subcommand)]
enum McpAction {
    /// Register Mizpah in Cursor, Claude Desktop, Claude Code, and Codex configs
    Install,
    /// Remove Mizpah MCP entries from those configs
    Uninstall,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Mcp { action, host, port }) => match action {
            None => {
                // stderr only — stdout is the MCP JSON-RPC channel
                tracing_subscriber::fmt()
                    .with_env_filter(
                        EnvFilter::try_from_default_env()
                            .unwrap_or_else(|_| EnvFilter::new("error")),
                    )
                    .with_writer(std::io::stderr)
                    .init();

                let base_url = mcp::hub_base_url(&host, port);
                if let Err(err) = mcp::run_stdio(base_url).await {
                    error!(error = %err, "MCP server failed");
                    std::process::exit(1);
                }
            }
            Some(McpAction::Install) => {
                std::process::exit(mcp::run_install());
            }
            Some(McpAction::Uninstall) => {
                std::process::exit(mcp::run_uninstall());
            }
        },
        None => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("error")),
                )
                .with_writer(std::io::stderr)
                .init();

            run_pipe_mode(cli).await;
        }
    }
}

async fn run_pipe_mode(cli: Cli) {
    let service = match cli.service.as_deref().map(str::trim) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            eprintln!("error: --service is required\n\nUsage: mizpah --service <name>\n       mizpah mcp\n       mizpah mcp install");
            std::process::exit(2);
        }
    };

    let addr: SocketAddr = format!("{}:{}", cli.host, cli.port)
        .parse()
        .unwrap_or_else(|e| {
            eprintln!("error: invalid host/port: {e}");
            std::process::exit(2);
        });

    match try_bind(addr) {
        Ok(listener) => {
            if let Err(err) = run_hub(
                listener,
                &cli.host,
                cli.port,
                cli.max_bytes,
                cli.no_open,
                service,
            )
            .await
            {
                error!(error = %err, "hub failed");
                std::process::exit(1);
            }
        }
        Err(bind_err) => {
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
            if let Err(err) = attach::attach_and_forward(&base_url, &service).await {
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
    host: &str,
    port: u16,
    max_bytes: u64,
    no_open: bool,
    service: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    std_listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(std_listener)?;

    let store = Arc::new(Store::new(max_bytes));
    let state = AppState {
        store: Arc::clone(&store),
    };
    let app = api::router(state);

    let url = format!("http://{host}:{port}");
    print_startup_banner(&url);

    // Auto-register MCP with local AI clients (idempotent).
    mcp::ensure_registered_on_hub_start();

    if !no_open {
        if let Err(err) = open::that(&url) {
            tracing::warn!(error = %err, "failed to open browser");
        }
    }

    let ingest_store = Arc::clone(&store);
    tokio::spawn(async move {
        ingest::ingest_stdin_local(ingest_store, service).await;
    });

    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn clap_accepts_mcp_without_service() {
        let cli = Cli::try_parse_from(["mizpah", "mcp"]).expect("mcp should parse");
        match cli.command {
            Some(Commands::Mcp { action: None, .. }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn clap_accepts_mcp_install() {
        let cli = Cli::try_parse_from(["mizpah", "mcp", "install"]).unwrap();
        match cli.command {
            Some(Commands::Mcp {
                action: Some(McpAction::Install),
                ..
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn clap_pipe_mode_still_works() {
        let cli = Cli::try_parse_from(["mizpah", "--service", "api", "--no-open"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.service.as_deref(), Some("api"));
        assert!(cli.no_open);
    }

    #[test]
    fn clap_help_renders() {
        let mut cmd = Cli::command();
        let help = cmd.render_help().to_string();
        assert!(help.contains("mcp"));
    }
}
