mod api;
mod attach;
mod filter;
mod ingest;
mod investigate;
mod mcp;
mod models;
mod pretty_ingest;
mod properties;
mod shell_attach;
mod shell_forward;
mod stdin_lines;
mod store;

use api::AppState;
use clap::{Parser, Subcommand};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use store::{Store, DEFAULT_MAX_BYTES};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

const DEFAULT_SERVICE: &str = "default";

#[derive(Debug, Parser)]
#[command(
    about = "JSON log viewer — pipe logs and inspect them in a web UI",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Service name for this ingest stream (defaults to absolute cwd)
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

    /// Project directory for "Check with Claude/Cursor" agent sessions (hub only)
    #[arg(long, env = "MIZPAH_PROJECT")]
    project: Option<PathBuf>,
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
    /// Capture stdout/stderr from new interactive shells into Mizpah
    Attach {
        /// Shared service name for all hooked shells (default: absolute cwd per command)
        #[arg(short, long)]
        service: Option<String>,

        /// Hub host
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// Hub port
        #[arg(short, long, default_value_t = 1738)]
        port: u16,
    },
    /// Stop capturing shell stdout/stderr (hub stays up)
    Detach,
    /// Start, stop, or restart the background hub
    Hub {
        #[command(subcommand)]
        action: HubAction,

        /// Hub host
        #[arg(long, default_value = "127.0.0.1", global = true)]
        host: String,

        /// Hub port
        #[arg(short, long, default_value_t = 1738, global = true)]
        port: u16,

        /// Project directory for "Check with Claude/Cursor" (start/restart only)
        #[arg(long, env = "MIZPAH_PROJECT", global = true)]
        project: Option<PathBuf>,
    },
    /// Open the Mizpah web UI in a browser
    Open {
        /// Hub host (defaults to attach state, then 127.0.0.1)
        #[arg(long)]
        host: Option<String>,

        /// Hub port (defaults to attach state, then 1738)
        #[arg(short, long)]
        port: Option<u16>,
    },
    /// Print shell init snippet for rc files (internal)
    #[command(name = "__shell-init", hide = true)]
    ShellInit {
        /// Shell kind: zsh or bash
        shell: String,
    },
    /// Forward stdin lines to the hub (internal; used by shell hooks)
    #[command(name = "__shell-forward", hide = true)]
    ShellForward {
        /// Initial service fallback (absolute cwd) until a per-command control frame arrives
        #[arg(long)]
        tty_service: String,
    },
}

#[derive(Debug, Subcommand)]
enum McpAction {
    /// Register Mizpah in Cursor, Claude Desktop, Claude Code, and Codex configs
    Install,
    /// Remove Mizpah MCP entries from those configs
    Uninstall,
}

#[derive(Debug, Subcommand)]
enum HubAction {
    /// Start a detached hub if one is not already healthy
    Start,
    /// Stop the hub tracked by the PID file for this port
    Stop,
    /// Stop then start the hub (clears the in-memory buffer)
    Restart,
}

fn init_tracing_stderr() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("error")),
        )
        .with_writer(std::io::stderr)
        .init();
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Mcp { action, host, port }) => match action {
            None => {
                // stderr only — stdout is the MCP JSON-RPC channel
                init_tracing_stderr();

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
        Some(Commands::Attach {
            service,
            host,
            port,
        }) => {
            init_tracing_stderr();
            if let Err(err) = shell_attach::run_attach(service, host, port).await {
                eprintln!("error: {err}");
                std::process::exit(1);
            }
        }
        Some(Commands::Detach) => {
            init_tracing_stderr();
            if let Err(err) = shell_attach::run_detach() {
                eprintln!("error: {err}");
                std::process::exit(1);
            }
        }
        Some(Commands::Hub {
            action,
            host,
            port,
            project,
        }) => {
            init_tracing_stderr();
            let result = match action {
                HubAction::Start => shell_attach::run_hub_start(host, port, project).await,
                HubAction::Stop => shell_attach::run_hub_stop(host, port).await,
                HubAction::Restart => shell_attach::run_hub_restart(host, port, project).await,
            };
            if let Err(err) = result {
                eprintln!("error: {err}");
                std::process::exit(1);
            }
        }
        Some(Commands::Open { host, port }) => {
            init_tracing_stderr();
            let (host, port) = match shell_attach::resolve_open_target(host, port) {
                Ok(t) => t,
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            };
            if let Err(err) = shell_attach::run_open(host, port).await {
                eprintln!("error: {err}");
                std::process::exit(1);
            }
        }
        Some(Commands::ShellInit { shell }) => {
            // stdout is evaluated by the shell — keep quiet on stderr unless error
            if let Err(err) = shell_attach::run_shell_init(&shell) {
                eprintln!("error: {err}");
                std::process::exit(1);
            }
        }
        Some(Commands::ShellForward { tty_service }) => {
            init_tracing_stderr();
            if let Err(err) = shell_forward::run_shell_forward(tty_service).await {
                eprintln!("error: {err}");
                std::process::exit(1);
            }
        }
        None => {
            init_tracing_stderr();
            run_pipe_mode(cli).await;
        }
    }
}

fn resolve_service(service: Option<&str>) -> String {
    if let Some(s) = service {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    service_from_cwd()
}

fn service_from_cwd() -> String {
    match std::env::current_dir() {
        Ok(dir) => match dir.canonicalize() {
            Ok(canon) => canon.display().to_string(),
            Err(_) if dir.is_absolute() => dir.display().to_string(),
            Err(_) => DEFAULT_SERVICE.to_string(),
        },
        Err(_) => DEFAULT_SERVICE.to_string(),
    }
}

fn resolve_project_dir(project: Option<PathBuf>) -> PathBuf {
    if let Some(p) = project {
        return p.canonicalize().unwrap_or(p);
    }
    match std::env::current_dir() {
        Ok(dir) => dir.canonicalize().unwrap_or(dir),
        Err(_) => PathBuf::from("."),
    }
}

async fn run_pipe_mode(cli: Cli) {
    let service = resolve_service(cli.service.as_deref());

    let addr: SocketAddr = format!("{}:{}", cli.host, cli.port)
        .parse()
        .unwrap_or_else(|e| {
            eprintln!("error: invalid host/port: {e}");
            std::process::exit(2);
        });

    let project_dir = resolve_project_dir(cli.project);

    match try_bind(addr) {
        Ok(listener) => {
            if let Err(err) = run_hub(
                listener,
                &cli.host,
                cli.port,
                cli.max_bytes,
                cli.no_open,
                service,
                project_dir,
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
    project_dir: PathBuf,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    std_listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(std_listener)?;

    if let Err(err) = shell_attach::write_hub_pid(port) {
        tracing::warn!(error = %err, port, "failed to write hub PID file");
    }

    let store = Arc::new(Store::new(max_bytes));
    let state = AppState {
        store: Arc::clone(&store),
        project_dir,
    };
    let app = api::router(state);

    let url = format!("http://{host}:{port}");
    print_startup_banner(&url);

    let ingest_store = Arc::clone(&store);
    tokio::spawn(async move {
        ingest::ingest_stdin_local(ingest_store, service).await;
    });

    // Serve immediately; MCP register + browser open must not delay readiness.
    let side_url = url.clone();
    tokio::spawn(async move {
        let _ = tokio::task::spawn_blocking(mcp::ensure_registered_on_hub_start).await;
        if !no_open {
            if let Err(err) = open::that(&side_url) {
                tracing::warn!(error = %err, "failed to open browser");
            }
        }
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
    fn clap_accepts_project_flag() {
        let cli = Cli::try_parse_from([
            "mizpah",
            "--project",
            "/tmp/my-app",
            "--no-open",
        ])
        .unwrap();
        assert_eq!(
            cli.project.as_deref(),
            Some(std::path::Path::new("/tmp/my-app"))
        );
        let resolved = resolve_project_dir(cli.project);
        assert!(resolved.ends_with("my-app") || resolved == PathBuf::from("/tmp/my-app"));
    }

    #[test]
    fn clap_pipe_mode_without_service_defaults_to_cwd() {
        let cli = Cli::try_parse_from(["mizpah", "--no-open"]).unwrap();
        assert!(cli.command.is_none());
        assert!(cli.service.is_none());
        let resolved = resolve_service(cli.service.as_deref());
        let cwd = std::env::current_dir()
            .ok()
            .and_then(|d| d.canonicalize().ok())
            .map(|d| d.display().to_string());
        if let Some(cwd) = cwd {
            assert_eq!(resolved, cwd);
        } else {
            assert_eq!(resolved, DEFAULT_SERVICE);
        }
    }

    #[test]
    fn resolve_service_trims_and_falls_back_to_cwd() {
        assert_eq!(resolve_service(Some("api")), "api");
        assert_eq!(resolve_service(Some("  api  ")), "api");
        let from_empty = resolve_service(Some(""));
        let from_ws = resolve_service(Some("   "));
        let from_none = resolve_service(None);
        assert_eq!(from_empty, from_none);
        assert_eq!(from_ws, from_none);
        assert!(!from_none.is_empty());
    }

    #[test]
    fn clap_help_renders() {
        let mut cmd = Cli::command();
        let help = cmd.render_help().to_string();
        assert!(help.contains("mcp"));
        assert!(help.contains("attach"));
        assert!(help.contains("detach"));
        assert!(help.contains("hub"));
        assert!(help.contains("open"));
    }

    #[test]
    fn clap_accepts_hub_start_stop_restart() {
        let start = Cli::try_parse_from([
            "mizpah",
            "hub",
            "start",
            "--host",
            "127.0.0.1",
            "-p",
            "1738",
        ])
        .unwrap();
        match start.command {
            Some(Commands::Hub {
                action: HubAction::Start,
                port: 1738,
                project: None,
                ..
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }

        let stop = Cli::try_parse_from(["mizpah", "hub", "stop", "--port", "9999"]).unwrap();
        match stop.command {
            Some(Commands::Hub {
                action: HubAction::Stop,
                port: 9999,
                ..
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }

        let restart = Cli::try_parse_from([
            "mizpah",
            "hub",
            "restart",
            "--project",
            "/tmp/my-app",
        ])
        .unwrap();
        match restart.command {
            Some(Commands::Hub {
                action: HubAction::Restart,
                project: Some(p),
                ..
            }) => assert_eq!(p, PathBuf::from("/tmp/my-app")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn clap_accepts_attach_detach_open() {
        let attach = Cli::try_parse_from([
            "mizpah",
            "attach",
            "--service",
            "dev",
            "--host",
            "127.0.0.1",
            "-p",
            "1738",
        ])
        .unwrap();
        match attach.command {
            Some(Commands::Attach {
                service: Some(s),
                port: 1738,
                ..
            }) => assert_eq!(s, "dev"),
            other => panic!("unexpected: {other:?}"),
        }

        let detach = Cli::try_parse_from(["mizpah", "detach"]).unwrap();
        assert!(matches!(detach.command, Some(Commands::Detach)));

        let open = Cli::try_parse_from(["mizpah", "open", "--port", "9999"]).unwrap();
        match open.command {
            Some(Commands::Open {
                host: None,
                port: Some(9999),
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn clap_accepts_hidden_shell_commands() {
        let init = Cli::try_parse_from(["mizpah", "__shell-init", "zsh"]).unwrap();
        match init.command {
            Some(Commands::ShellInit { shell }) => assert_eq!(shell, "zsh"),
            other => panic!("unexpected: {other:?}"),
        }

        let fwd =
            Cli::try_parse_from(["mizpah", "__shell-forward", "--tty-service", "ttys001"]).unwrap();
        match fwd.command {
            Some(Commands::ShellForward { tty_service }) => assert_eq!(tty_service, "ttys001"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn clap_rejects_invalid_shell_init_arity() {
        assert!(Cli::try_parse_from(["mizpah", "__shell-init"]).is_err());
    }
}
