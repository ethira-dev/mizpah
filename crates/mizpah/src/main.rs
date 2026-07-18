mod agent_hooks;
mod api;
mod attach;
mod browser_attach;
mod cli;
mod error;
mod filter;
mod hub;
mod ingest;
mod ingest_forward;
mod investigate;
mod mcp;
mod models;
mod mzp_meta;
mod pretty_ingest;
mod properties;
mod shell_attach;
mod shell_forward;
mod stdin_lines;
mod store;
mod unix_process;
mod update;
mod util;

use api::AppState;
use cli::Cli;
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use store::Store;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

const DEFAULT_SERVICE: &str = "default";

#[tokio::main]
async fn main() {
    cli::run().await;
}

pub(crate) fn init_tracing_stderr() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
}

/// Returns true when `host` is clearly loopback (name or literal).
fn is_bind_loopback(host: &str) -> bool {
    let h = host.trim();
    if h.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match h.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => false,
    }
}

pub(crate) fn ensure_bind_allowed(host: &str, allow_remote: bool) {
    if is_bind_loopback(host) {
        return;
    }
    if allow_remote {
        eprintln!(
            "warning: binding hub on non-loopback address {host:?}\n\
             The hub has no authentication: anyone who can reach it can ingest logs,\n\
             query data, and trigger investigate/update. Prefer SSH tunnels or a reverse\n\
             proxy with auth when exposing Mizpah beyond this machine."
        );
        return;
    }
    eprintln!(
        "error: refusing to bind hub on non-loopback address {host:?}\n\
         hint: use --host 127.0.0.1 (default), or pass --allow-remote if you understand\n\
         that ingest/API are unauthenticated on the bound interface"
    );
    std::process::exit(2);
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

pub(crate) async fn run_pipe_mode(cli: Cli) {
    let service = resolve_service(cli.service.as_deref());

    let addr: SocketAddr = format!("{}:{}", cli.hub.host, cli.hub.port)
        .parse()
        .unwrap_or_else(|e| {
            eprintln!("error: invalid host/port: {e}");
            std::process::exit(2);
        });

    let project_dir = resolve_project_dir(cli.project);

    match try_bind(addr) {
        Ok(listener) => {
            // Only enforce when we actually become the hub (bind succeeded).
            ensure_bind_allowed(&cli.hub.host, cli.allow_remote);
            if let Err(err) = run_hub(
                listener,
                &cli.hub.host,
                cli.hub.port,
                cli.max_bytes,
                cli.ttl_hours,
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

            let base_url = format!("http://{}:{}", cli.hub.host, cli.hub.port);
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

#[allow(clippy::too_many_arguments)]
async fn run_hub(
    std_listener: TcpListener,
    host: &str,
    port: u16,
    max_bytes: u64,
    ttl_hours: u64,
    no_open: bool,
    service: String,
    project_dir: PathBuf,
) -> Result<(), error::HubLifecycleError> {
    std_listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(std_listener)?;

    if let Err(err) = hub::write_hub_pid(port) {
        tracing::warn!(error = %err, port, "failed to write hub PID file");
    }

    let store = Arc::new(Store::with_ttl_hours(max_bytes, ttl_hours));
    match store.restore_update_spill().await {
        Ok(0) => {}
        Ok(n) => info!(restored = n, "restored log buffer from update spill"),
        Err(err) => tracing::warn!(error = %err, "failed to restore update spill"),
    }
    let update_mgr = update::UpdateManager::new(update::RestartContext {
        host: host.to_string(),
        port,
        project_dir: project_dir.clone(),
        max_bytes,
        ttl_hours,
    });
    update_mgr.spawn_background_checker();
    let state = AppState {
        store: Arc::clone(&store),
        project_dir,
        update: update_mgr,
    };
    let app = api::router(state);

    let url = format!("http://{host}:{port}");
    print_startup_banner(&url);

    let ingest_store = Arc::clone(&store);
    tokio::spawn(async move {
        ingest::ingest_stdin_local(ingest_store, service).await;
    });

    if ttl_hours > 0 {
        let ttl_store = Arc::clone(&store);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let _ = ttl_store.expire_ttl().await;
            }
        });
    }

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

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use cli::Cli;

    #[test]
    fn clap_accepts_project_flag_resolves() {
        let cli = Cli::try_parse_from(["mizpah", "--project", "/tmp/my-app", "--no-open"]).unwrap();
        let resolved = resolve_project_dir(cli.project);
        assert!(resolved.ends_with("my-app") || resolved == std::path::Path::new("/tmp/my-app"));
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
}
