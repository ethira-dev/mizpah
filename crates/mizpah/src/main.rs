mod agent_hooks;
mod api;
mod attach;
mod browser_attach;
mod cli;
mod config;
mod error;
mod event_time;
mod file_ingest;
mod filter;
mod formats;
mod hub;
mod ingest;
mod ingest_forward;
mod investigate;
mod keymap;
mod mcp;
mod models;
mod mzp_meta;
mod pretty_ingest;
mod properties;
mod run_cmd;
mod script;
mod service;
mod setup;
mod shell_attach;
mod shell_forward;
mod sql;
mod stdin_lines;
mod store;
mod tui;
mod unix_process;
mod update;
mod util;
mod nl_cel;
mod incident;
mod session;

#[cfg(test)]
mod test_support;

use api::AppState;
use cli::Cli;
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use store::Store;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

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

/// Validate bind host policy. Returns `Err(message)` when bind must be refused.
pub(crate) fn check_bind_allowed(host: &str, allow_remote: bool) -> Result<(), String> {
    if is_bind_loopback(host) {
        return Ok(());
    }
    if allow_remote {
        eprintln!(
            "warning: binding hub on non-loopback address {host:?}\n\
             The hub has no authentication: anyone who can reach it can ingest logs,\n\
             query data, and trigger investigate/update. Prefer SSH tunnels or a reverse\n\
             proxy with auth when exposing Mizpah beyond this machine."
        );
        return Ok(());
    }
    Err(format!(
        "error: refusing to bind hub on non-loopback address {host:?}\n\
         hint: use --host 127.0.0.1 (default), or pass --allow-remote if you understand\n\
         that ingest/API are unauthenticated on the bound interface"
    ))
}

/// Validate bind policy, printing the refusal message on error (no process exit).
pub(crate) fn ensure_bind_allowed_result(host: &str, allow_remote: bool) -> Result<(), String> {
    check_bind_allowed(host, allow_remote).map_err(|msg| {
        eprintln!("{msg}");
        msg
    })
}

pub(crate) use service::resolve_service;

fn resolve_project_dir(project: Option<PathBuf>) -> PathBuf {
    if let Some(p) = project {
        return p.canonicalize().unwrap_or(p);
    }
    match std::env::current_dir() {
        Ok(dir) => dir.canonicalize().unwrap_or(dir),
        Err(_) => PathBuf::from("."),
    }
}

/// Pipe-mode control flow without `process::exit` (CLI maps `Err(code)` to process exit).
pub(crate) async fn run_pipe_mode(cli: Cli) -> Result<(), i32> {
    run_pipe_mode_with(
        cli,
        try_bind,
        |listener, host, port, max_bytes, ttl_hours, no_open, service, project_dir| async move {
            run_hub(
                listener,
                &host,
                port,
                max_bytes,
                ttl_hours,
                no_open,
                service,
                project_dir,
            )
            .await
            .map_err(|e| e.to_string())
        },
        |base_url, service| async move {
            attach::attach_and_forward(&base_url, &service)
                .await
                .map_err(|e| e.to_string())
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_pipe_mode_with<B, H, HF, A, AF>(
    cli: Cli,
    bind: B,
    on_hub: H,
    on_attach: A,
) -> Result<(), i32>
where
    B: FnOnce(SocketAddr) -> std::io::Result<TcpListener>,
    H: FnOnce(TcpListener, String, u16, u64, u64, bool, String, PathBuf) -> HF,
    HF: std::future::Future<Output = Result<(), String>>,
    A: FnOnce(String, String) -> AF,
    AF: std::future::Future<Output = Result<(), String>>,
{
    let service = resolve_service(cli.service.as_deref());

    let addr: SocketAddr = match format!("{}:{}", cli.hub.host, cli.hub.port).parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: invalid host/port: {e}");
            return Err(2);
        }
    };

    let project_dir = resolve_project_dir(cli.project);
    let host = cli.hub.host.clone();
    let port = cli.hub.port;
    let max_bytes = cli.max_bytes;
    let ttl_hours = cli.ttl_hours;
    let no_open = cli.no_open;
    let allow_remote = cli.allow_remote;

    match bind(addr) {
        Ok(listener) => {
            if ensure_bind_allowed_result(&host, allow_remote).is_err() {
                return Err(2);
            }
            if let Err(err) = on_hub(
                listener,
                host,
                port,
                max_bytes,
                ttl_hours,
                no_open,
                service,
                project_dir,
            )
            .await
            {
                error!(error = %err, "hub failed");
                return Err(1);
            }
            Ok(())
        }
        Err(bind_err) => {
            let in_use = bind_err.kind() == std::io::ErrorKind::AddrInUse;
            if !in_use {
                eprintln!(
                    "error: could not bind {addr}: {bind_err}\n\
                     hint: if another Mizpah hub should be used, ensure it is reachable"
                );
                return Err(1);
            }

            let base_url = format!("http://{host}:{port}");
            info!(%addr, "port in use; attaching as ingest client");
            if let Err(err) = on_attach(base_url, service).await {
                eprintln!("error: {err}");
                return Err(1);
            }
            Ok(())
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
    if let Some(persist_dir) = crate::config::MizpahConfig::load().resolve_persist_dir() {
        match store.hydrate_from_persist(&persist_dir).await {
            Ok(0) => {}
            Ok(n) => info!(restored = n, path = %persist_dir.display(), "hydrated from persist"),
            Err(err) => tracing::warn!(error = %err, "persist hydrate failed"),
        }
        if let Err(err) = store.enable_persist(&persist_dir).await {
            tracing::warn!(error = %err, "persist enable failed");
        }
    }
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
    use std::sync::Mutex;

    static SERVICE_ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        key: &'static str,
        old: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn clear(key: &'static str) -> Self {
            let old = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.old.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn clear_service_env() -> (EnvGuard, EnvGuard, EnvGuard) {
        (
            EnvGuard::clear("MIZPAH_SERVICE"),
            EnvGuard::clear("OTEL_SERVICE_NAME"),
            EnvGuard::clear("SERVICE_NAME"),
        )
    }

    #[test]
    fn clap_accepts_project_flag_resolves() {
        let cli = Cli::try_parse_from(["mizpah", "--project", "/tmp/my-app", "--no-open"]).unwrap();
        let resolved = resolve_project_dir(cli.project);
        assert!(resolved.ends_with("my-app") || resolved == std::path::Path::new("/tmp/my-app"));
    }

    #[test]
    fn clap_pipe_mode_without_service_defaults_to_cwd() {
        let _lock = SERVICE_ENV_LOCK.lock().unwrap();
        let _env = clear_service_env();
        let cli = Cli::try_parse_from(["mizpah", "--no-open"]).unwrap();
        assert!(cli.command.is_none());
        assert!(cli.service.is_none());
        let resolved = resolve_service(cli.service.as_deref());
        assert!(!resolved.is_empty());
        assert!(
            !resolved.contains('/'),
            "expected short inferred slug, got {resolved:?}"
        );
    }

    #[test]
    fn resolve_service_trims_and_falls_back_to_cwd() {
        let _lock = SERVICE_ENV_LOCK.lock().unwrap();
        let _env = clear_service_env();
        assert_eq!(resolve_service(Some("api")), "api");
        assert_eq!(resolve_service(Some("  api  ")), "api");
        let from_empty = resolve_service(Some(""));
        let from_ws = resolve_service(Some("   "));
        let from_none = resolve_service(None);
        assert_eq!(from_empty, from_none);
        assert_eq!(from_ws, from_none);
        assert!(!from_none.is_empty());
        assert!(
            !from_none.contains('/'),
            "expected short inferred slug, got {from_none:?}"
        );
    }

    #[test]
    fn bind_loopback_hosts() {
        assert!(is_bind_loopback("127.0.0.1"));
        assert!(is_bind_loopback("localhost"));
        assert!(is_bind_loopback("::1"));
        assert!(!is_bind_loopback("0.0.0.0"));
        assert!(!is_bind_loopback("192.168.1.1"));
    }

    #[test]
    fn check_bind_allowed_policy() {
        assert!(check_bind_allowed("127.0.0.1", false).is_ok());
        assert!(check_bind_allowed("0.0.0.0", true).is_ok());
        assert!(check_bind_allowed("0.0.0.0", false).is_err());
    }

    #[test]
    fn print_startup_banner_smoke() {
        print_startup_banner("http://127.0.0.1:3149");
    }

    #[test]
    fn try_bind_ephemeral_port() {
        let listener = try_bind("127.0.0.1:0".parse().unwrap()).unwrap();
        assert!(listener.local_addr().is_ok());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn run_hub_serves_stats_then_abort() {
        crate::util::ensure_rustls_crypto_provider();
        let _guard = crate::test_support::env_lock();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("MIZPAH_CONFIG_DIR", dir.path());

        let std_listener = try_bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let port = std_listener.local_addr().unwrap().port();
        let project = dir.path().to_path_buf();

        let hub = tokio::spawn(async move {
            run_hub(
                std_listener,
                "127.0.0.1",
                port,
                1_000_000,
                0,
                true, // no_open
                "test".into(),
                project,
            )
            .await
        });

        // Wait until hub answers.
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{port}/api/stats");
        let mut ok = false;
        for _ in 0..50 {
            if let Ok(resp) = client.get(&url).send().await {
                if resp.status().is_success() {
                    ok = true;
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(ok, "hub did not become ready");

        hub.abort();
        let _ = hub.await;
        std::env::remove_var("MIZPAH_CONFIG_DIR");
    }

    #[test]
    fn try_bind_port_in_use() {
        let listener = try_bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = listener.local_addr().unwrap();
        let result = try_bind(addr);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::AddrInUse);
    }

    #[test]
    fn resolve_project_dir_defaults() {
        let resolved = resolve_project_dir(None);
        assert!(resolved.is_absolute() || resolved == *".");
    }

    #[test]
    fn inferred_service_from_cwd_non_empty() {
        let _lock = SERVICE_ENV_LOCK.lock().unwrap();
        let _env = clear_service_env();
        let svc = resolve_service(None);
        assert!(!svc.is_empty());
        assert!(!svc.contains('/'));
    }

    #[test]
    fn resolve_project_dir_with_some() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("project");
        std::fs::create_dir(&path).unwrap();
        let resolved = resolve_project_dir(Some(path.clone()));
        assert!(resolved.ends_with("project") || resolved == path);
    }

    #[test]
    fn resolve_project_dir_canonicalizes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proj");
        std::fs::create_dir(&path).unwrap();
        let resolved = resolve_project_dir(Some(path));
        assert!(resolved.is_absolute());
    }

    fn pipe_cli(host: &str, port: u16, allow_remote: bool) -> Cli {
        Cli::try_parse_from(
            [
                "mizpah",
                "--no-open",
                "--host",
                host,
                "--port",
                &port.to_string(),
                "--service",
                "svc",
            ]
            .into_iter()
            .chain(allow_remote.then_some("--allow-remote")),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn pipe_mode_invalid_host_port() {
        let mut cli = pipe_cli("127.0.0.1", 1, false);
        cli.hub.host = "not a host!!!".into();
        let err = run_pipe_mode_with(
            cli,
            |_| unreachable!(),
            |_, _, _, _, _, _, _, _| async { Ok(()) },
            |_, _| async { Ok(()) },
        )
        .await;
        assert_eq!(err, Err(2));
    }

    #[tokio::test]
    async fn pipe_mode_bind_refused_non_loopback() {
        let cli = pipe_cli("0.0.0.0", 0, false);
        let err = run_pipe_mode_with(
            cli,
            |_| Ok(try_bind("127.0.0.1:0".parse().unwrap()).unwrap()),
            |_, _, _, _, _, _, _, _| async { Ok(()) },
            |_, _| async { Ok(()) },
        )
        .await;
        assert_eq!(err, Err(2));
    }

    #[tokio::test]
    async fn pipe_mode_hub_success_and_failure() {
        run_pipe_mode_with(
            pipe_cli("127.0.0.1", 0, false),
            |_| Ok(try_bind("127.0.0.1:0".parse().unwrap()).unwrap()),
            |_, _, _, _, _, _, _, _| async { Ok(()) },
            |_, _| async { Ok(()) },
        )
        .await
        .unwrap();

        let err = run_pipe_mode_with(
            pipe_cli("127.0.0.1", 0, false),
            |_| Ok(try_bind("127.0.0.1:0".parse().unwrap()).unwrap()),
            |_, _, _, _, _, _, _, _| async { Err("hub boom".into()) },
            |_, _| async { Ok(()) },
        )
        .await;
        assert_eq!(err, Err(1));
    }

    #[tokio::test]
    async fn pipe_mode_attach_when_addr_in_use() {
        let held = try_bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let port = held.local_addr().unwrap().port();
        let cli = pipe_cli("127.0.0.1", port, false);
        let attached = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = attached.clone();
        run_pipe_mode_with(
            cli,
            try_bind,
            |_, _, _, _, _, _, _, _| async { Ok(()) },
            move |_url, service| {
                let flag = flag;
                async move {
                    assert_eq!(service, "svc");
                    flag.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                }
            },
        )
        .await
        .unwrap();
        assert!(attached.load(std::sync::atomic::Ordering::SeqCst));
        drop(held);
    }

    #[tokio::test]
    async fn pipe_mode_attach_failure_and_other_bind_error() {
        let held = try_bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let port = held.local_addr().unwrap().port();
        let cli = pipe_cli("127.0.0.1", port, false);
        let err = run_pipe_mode_with(
            cli,
            try_bind,
            |_, _, _, _, _, _, _, _| async { Ok(()) },
            |_, _| async { Err("attach failed".into()) },
        )
        .await;
        assert_eq!(err, Err(1));
        drop(held);

        let cli = pipe_cli("127.0.0.1", 1, false);
        let err = run_pipe_mode_with(
            cli,
            |_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "nope",
                ))
            },
            |_, _, _, _, _, _, _, _| async { Ok(()) },
            |_, _| async { Ok(()) },
        )
        .await;
        assert_eq!(err, Err(1));
    }

    #[test]
    fn ensure_bind_allowed_result_ok_and_err() {
        assert!(ensure_bind_allowed_result("127.0.0.1", false).is_ok());
        assert!(ensure_bind_allowed_result("0.0.0.0", true).is_ok());
        assert!(ensure_bind_allowed_result("0.0.0.0", false).is_err());
    }

    #[test]
    fn init_tracing_stderr_smoke() {
        // May already be initialized by other tests; ignore SetGlobalDefault errors via catch.
        let _ = std::panic::catch_unwind(init_tracing_stderr);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn run_pipe_mode_attaches_to_live_hub() {
        crate::util::ensure_rustls_crypto_provider();
        let (url, _store) = crate::test_support::spawn_test_hub().await;
        let parsed = url::Url::parse(&url).unwrap();
        let port = parsed.port().unwrap();
        let cli = pipe_cli("127.0.0.1", port, false);
        // Empty stdin → attach_and_forward returns on EOF.
        run_pipe_mode(cli).await.unwrap();
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn run_hub_with_ttl_and_persist_config() {
        crate::util::ensure_rustls_crypto_provider();
        let _guard = crate::test_support::env_lock();
        let dir = tempfile::tempdir().unwrap();
        let persist = dir.path().join("persist");
        std::fs::create_dir_all(&persist).unwrap();
        std::env::set_var("MIZPAH_CONFIG_DIR", dir.path());
        // Minimal config enabling persist under config dir.
        std::fs::write(
            dir.path().join("config.toml"),
            format!("persist_dir = \"{}\"\n", persist.display()),
        )
        .unwrap();

        let std_listener = try_bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let port = std_listener.local_addr().unwrap().port();
        let project = dir.path().to_path_buf();
        let hub = tokio::spawn(async move {
            run_hub(
                std_listener,
                "127.0.0.1",
                port,
                1_000_000,
                1, // enable TTL sweeper spawn
                true,
                "ttl-test".into(),
                project,
            )
            .await
        });

        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{port}/api/stats");
        for _ in 0..50 {
            if let Ok(resp) = client.get(&url).send().await {
                if resp.status().is_success() {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }
        hub.abort();
        let _ = hub.await;
        std::env::remove_var("MIZPAH_CONFIG_DIR");
    }
}
