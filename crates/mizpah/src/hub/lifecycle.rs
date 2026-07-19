//! Hub health checks, lifecycle, and detached spawn.

use crate::error::HubLifecycleError;
use crate::hub::pid::{read_hub_pid, remove_hub_pid};
use crate::unix_process;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

const HUB_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const HUB_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const HUB_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub fn hub_url(host: &str, port: u16) -> String {
    format!("http://{host}:{port}")
}

pub async fn probe_hub(host: &str, port: u16) -> bool {
    crate::util::ensure_rustls_crypto_provider();
    let url = format!("{}/api/stats", hub_url(host, port));
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    match client.get(&url).send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Ensure a healthy hub is reachable. Spawns a detached hub for loopback hosts, or for
/// non-loopback hosts when `allow_remote` is set (unauthenticated bind — use with care).
/// When `project` is `None`, the current working directory is used (same as attach).
pub async fn ensure_hub(
    host: &str,
    port: u16,
    project: Option<&Path>,
    allow_remote: bool,
) -> Result<(), HubLifecycleError> {
    ensure_hub_impl(
        host,
        port,
        project,
        allow_remote,
        Arc::new(|host: &str, port: u16| {
            let host = host.to_string();
            Box::pin(async move { probe_hub(&host, port).await })
                as std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        }),
        Arc::new(
            |exe: &Path, host: &str, port: u16, project: Option<&Path>, allow_remote: bool| {
                spawn_detached_hub(exe, host, port, project, allow_remote)
            },
        ),
        Arc::new(|| crate::mcp::resolve_binary_path().map_err(|e| e.to_string())),
        HUB_STARTUP_TIMEOUT,
    )
    .await
}

type ResolveBinaryFn = Arc<dyn Fn() -> Result<PathBuf, String> + Send + Sync>;

type HubProbeFn = Arc<
    dyn Fn(&str, u16) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        + Send
        + Sync,
>;
type DetachedHubSpawner = Arc<
    dyn Fn(&Path, &str, u16, Option<&Path>, bool) -> io::Result<std::process::Child> + Send + Sync,
>;

#[allow(clippy::too_many_arguments)]
async fn ensure_hub_impl(
    host: &str,
    port: u16,
    project: Option<&Path>,
    allow_remote: bool,
    probe: HubProbeFn,
    spawner: DetachedHubSpawner,
    resolve_binary: ResolveBinaryFn,
    startup_timeout: Duration,
) -> Result<(), HubLifecycleError> {
    if probe(host, port).await {
        return Ok(());
    }

    if !is_loopback_host(host) && !allow_remote {
        return Err(HubLifecycleError::msg(format!(
            "hub at {} is not reachable; start it remotely, use a loopback host, or pass --allow-remote",
            hub_url(host, port)
        )));
    }

    let exe = resolve_binary().map_err(HubLifecycleError::msg)?;

    let project_buf = project
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::current_dir().ok());
    let mut child = spawner(&exe, host, port, project_buf.as_deref(), allow_remote)
        .map_err(|e| HubLifecycleError::msg(format!("failed to start hub: {e}")))?;

    let deadline = tokio::time::Instant::now() + startup_timeout;
    while tokio::time::Instant::now() < deadline {
        if probe(host, port).await {
            let _ = child;
            return Ok(());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(HubLifecycleError::msg(format!(
                    "hub process exited early with {status}"
                )));
            }
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                return Err(HubLifecycleError::msg(format!(
                    "failed monitoring hub process: {e}"
                )));
            }
        }
        tokio::time::sleep(HUB_POLL_INTERVAL).await;
    }

    let _ = child.kill();
    let _ = child.wait();
    Err(HubLifecycleError::msg(format!(
        "hub at {} did not become healthy within {}s",
        hub_url(host, port),
        startup_timeout.as_secs()
    )))
}

/// Run `mzp hub start`.
pub async fn run_hub_start(
    host: String,
    port: u16,
    project: Option<PathBuf>,
    allow_remote: bool,
) -> Result<(), HubLifecycleError> {
    let url = hub_url(&host, port);
    if probe_hub(&host, port).await {
        eprintln!("mizpah hub already running at {url}");
        return Ok(());
    }
    ensure_hub(&host, port, project.as_deref(), allow_remote).await?;
    eprintln!("mizpah hub started at {url}");
    Ok(())
}

/// Run `mzp hub stop`.
pub async fn run_hub_stop(host: String, port: u16) -> Result<(), HubLifecycleError> {
    let url = hub_url(&host, port);
    let pid = match read_hub_pid(port) {
        Ok(Some(p)) => p,
        Ok(None) => {
            if probe_hub(&host, port).await {
                return Err(HubLifecycleError::msg(format!(
                    "hub at {url} appears running but PID file is missing\n\
                     hint: stop the process listening on port {port} manually, then retry"
                )));
            }
            eprintln!("mizpah hub already stopped");
            return Ok(());
        }
        Err(e) => {
            return Err(HubLifecycleError::msg(format!(
                "failed to read hub PID file: {e}"
            )))
        }
    };

    if !unix_process::process_exists(pid) {
        let _ = remove_hub_pid(port);
        if probe_hub(&host, port).await {
            return Err(HubLifecycleError::msg(format!(
                "hub at {url} appears running but PID file is stale (pid {pid})\n\
                 hint: stop the process listening on port {port} manually, then retry"
            )));
        }
        eprintln!("mizpah hub already stopped (stale PID file removed)");
        return Ok(());
    }

    unix_process::signal_term(pid).map_err(HubLifecycleError::msg)?;

    let deadline = tokio::time::Instant::now() + HUB_STOP_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if !unix_process::process_exists(pid) && !probe_hub(&host, port).await {
            break;
        }
        if !unix_process::process_exists(pid) {
            break;
        }
        tokio::time::sleep(HUB_POLL_INTERVAL).await;
    }

    if unix_process::process_exists(pid) {
        unix_process::signal_kill(pid).map_err(HubLifecycleError::msg)?;
        let kill_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < kill_deadline {
            if !unix_process::process_exists(pid) {
                break;
            }
            tokio::time::sleep(HUB_POLL_INTERVAL).await;
        }
    }

    let _ = remove_hub_pid(port);

    if unix_process::process_exists(pid) {
        return Err(HubLifecycleError::msg(format!(
            "hub process pid {pid} did not exit"
        )));
    }
    if probe_hub(&host, port).await {
        return Err(HubLifecycleError::msg(format!(
            "hub at {url} is still reachable after stopping pid {pid}"
        )));
    }

    eprintln!("mizpah hub stopped");
    Ok(())
}

/// Run `mzp hub restart`.
pub async fn run_hub_restart(
    host: String,
    port: u16,
    project: Option<PathBuf>,
    allow_remote: bool,
) -> Result<(), HubLifecycleError> {
    run_hub_stop(host.clone(), port).await?;
    run_hub_start(host, port, project, allow_remote).await
}

fn spawn_detached_hub(
    exe: &Path,
    host: &str,
    port: u16,
    project: Option<&Path>,
    allow_remote: bool,
) -> io::Result<std::process::Child> {
    spawn_detached_hub_with_options(exe, host, port, project, None, None, allow_remote)
}

/// Spawn a detached hub process. Optionally pass `--max-bytes` / `--ttl-hours` (used after self-update).
pub fn spawn_detached_hub_with_options(
    exe: &Path,
    host: &str,
    port: u16,
    project: Option<&Path>,
    max_bytes: Option<u64>,
    ttl_hours: Option<u64>,
    allow_remote: bool,
) -> io::Result<std::process::Child> {
    let mut cmd = Command::new(exe);
    cmd.args(["--host", host, "--port", &port.to_string(), "--no-open"]);
    if allow_remote {
        cmd.arg("--allow-remote");
    }
    if let Some(max_bytes) = max_bytes {
        cmd.arg("--max-bytes").arg(max_bytes.to_string());
    }
    if let Some(ttl_hours) = ttl_hours {
        cmd.arg("--ttl-hours").arg(ttl_hours.to_string());
    }
    if let Some(project) = project {
        cmd.arg("--project").arg(project);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    crate::unix_process::apply_pre_exec_setsid(&mut cmd);

    cmd.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_loopback() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("192.168.1.1"));
        assert!(!is_loopback_host("example.com"));
    }

    #[test]
    fn hub_url_format() {
        assert_eq!(hub_url("127.0.0.1", 3149), "http://127.0.0.1:3149");
        assert_eq!(hub_url("localhost", 8080), "http://localhost:8080");
    }

    #[tokio::test]
    async fn ensure_hub_already_up() {
        let (base_url, _store) = crate::test_support::spawn_test_hub().await;
        let parts: Vec<&str> = base_url.trim_start_matches("http://").split(':').collect();
        let host = parts[0];
        let port: u16 = parts[1].parse().unwrap();

        let result = ensure_hub(host, port, None, false).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn ensure_hub_remote_denied() {
        let result = ensure_hub("192.168.1.1", 9999, None, false).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not reachable") || err.contains("allow-remote"));
    }

    #[tokio::test]
    async fn run_hub_start_already_running() {
        let (base_url, _store) = crate::test_support::spawn_test_hub().await;
        let parts: Vec<&str> = base_url.trim_start_matches("http://").split(':').collect();
        let host = parts[0].to_string();
        let port: u16 = parts[1].parse().unwrap();

        let result = run_hub_start(host, port, None, false).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn run_hub_stop_not_running() {
        let result = run_hub_stop("127.0.0.1".to_string(), 19999).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn run_hub_stop_with_stale_pid() {
        use crate::hub::pid::{hub_pid_path, write_hub_pid};
        use crate::test_support::env_lock;

        let _guard = env_lock();
        let test_port = 19998u16;
        let dir = std::env::temp_dir().join(format!("mizpah-hub-stop-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let old = std::env::var_os("MIZPAH_CONFIG_DIR");
        std::env::set_var("MIZPAH_CONFIG_DIR", &dir);

        write_hub_pid(test_port).unwrap();

        let pid_path = hub_pid_path(test_port).unwrap();
        std::fs::write(&pid_path, "999999999\n").unwrap();

        let result = run_hub_stop("127.0.0.1".to_string(), test_port).await;

        match old {
            Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
            None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
        }
        let _ = std::fs::remove_dir_all(&dir);

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn run_hub_restart_not_running() {
        // Stop is a no-op when nothing is listening; start may fail if the
        // test binary cannot act as a hub — both outcomes exercise the path.
        let result = run_hub_restart("127.0.0.1".to_string(), 19997, None, false).await;
        let _ = result;
    }

    #[test]
    fn spawn_detached_hub_with_all_options() {
        let exe = std::env::current_exe().unwrap();
        let project = std::env::temp_dir();

        let result = spawn_detached_hub_with_options(
            &exe,
            "127.0.0.1",
            3149,
            Some(&project),
            Some(1024 * 1024),
            Some(24),
            false,
        );

        if let Ok(mut child) = result {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    #[test]
    fn spawn_detached_hub_allow_remote() {
        let exe = std::env::current_exe().unwrap();
        let result = spawn_detached_hub_with_options(&exe, "0.0.0.0", 3149, None, None, None, true);
        if let Ok(mut child) = result {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    #[tokio::test]
    async fn probe_hub_not_running() {
        let result = probe_hub("127.0.0.1", 19996).await;
        assert!(!result);
    }

    #[tokio::test]
    async fn probe_hub_running() {
        let (base_url, _store) = crate::test_support::spawn_test_hub().await;
        let parts: Vec<&str> = base_url.trim_start_matches("http://").split(':').collect();
        let host = parts[0];
        let port: u16 = parts[1].parse().unwrap();
        let result = probe_hub(host, port).await;
        assert!(result);
    }

    #[tokio::test]
    async fn run_hub_stop_running_without_pid_file() {
        let (base_url, _store) = crate::test_support::spawn_test_hub().await;
        let parts: Vec<&str> = base_url.trim_start_matches("http://").split(':').collect();
        let host = parts[0].to_string();
        let port: u16 = parts[1].parse().unwrap();
        let result = run_hub_stop(host, port).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("PID file is missing"));
    }

    #[tokio::test]
    async fn ensure_hub_impl_child_exits_early() {
        use std::process::{Command, Stdio};
        #[cfg(unix)]
        fn mock_child() -> std::process::Child {
            let true_bin = ["/usr/bin/false", "/bin/false"]
                .into_iter()
                .find(|p| Path::new(p).is_file())
                .expect("false binary");
            Command::new(true_bin)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        }
        #[cfg(not(unix))]
        fn mock_child() -> std::process::Child {
            panic!("test only runs on unix")
        }

        let probe = Arc::new(|_host: &str, _port: u16| {
            Box::pin(async { false })
                as std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        });
        let spawner = Arc::new(
            |_exe: &Path, _host: &str, _port: u16, _project: Option<&Path>, _allow_remote: bool| {
                Ok(mock_child())
            },
        );
        let resolve = Arc::new(|| Ok(std::env::current_exe().unwrap()));
        let result = ensure_hub_impl(
            "127.0.0.1",
            19995,
            None,
            false,
            probe,
            spawner,
            resolve,
            Duration::from_millis(200),
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("exited early"));
    }

    #[tokio::test]
    async fn ensure_hub_impl_becomes_healthy() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_clone = Arc::clone(&calls);
        let probe = Arc::new(move |_h: &str, _p: u16| {
            let calls = Arc::clone(&calls_clone);
            Box::pin(async move { calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 2 })
                as std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        });
        #[cfg(unix)]
        fn mock_child() -> std::process::Child {
            use std::process::{Command, Stdio};
            Command::new("/bin/sleep")
                .arg("30")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        }
        #[cfg(not(unix))]
        fn mock_child() -> std::process::Child {
            panic!("test only runs on unix")
        }
        let spawner = Arc::new(
            |_exe: &Path, _host: &str, _port: u16, _project: Option<&Path>, _allow_remote: bool| {
                Ok(mock_child())
            },
        );
        let resolve = Arc::new(|| Ok(std::env::current_exe().unwrap()));
        let result = ensure_hub_impl(
            "127.0.0.1",
            19994,
            None,
            false,
            probe,
            spawner,
            resolve,
            Duration::from_secs(2),
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn ensure_hub_impl_startup_timeout() {
        let probe = Arc::new(|_host: &str, _port: u16| {
            Box::pin(async { false })
                as std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        });
        #[cfg(unix)]
        fn mock_child() -> std::process::Child {
            use std::process::{Command, Stdio};
            Command::new("/bin/sleep")
                .arg("30")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        }
        #[cfg(not(unix))]
        fn mock_child() -> std::process::Child {
            panic!("test only runs on unix")
        }
        let spawner = Arc::new(
            |_exe: &Path, _host: &str, _port: u16, _project: Option<&Path>, _allow_remote: bool| {
                Ok(mock_child())
            },
        );
        let resolve = Arc::new(|| Ok(std::env::current_exe().unwrap()));
        let result = ensure_hub_impl(
            "127.0.0.1",
            19993,
            None,
            false,
            probe,
            spawner,
            resolve,
            Duration::from_millis(150),
        )
        .await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("did not become healthy"));
    }

    #[tokio::test]
    async fn ensure_hub_impl_resolve_binary_failure() {
        let probe = Arc::new(|_host: &str, _port: u16| {
            Box::pin(async { false })
                as std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        });
        let spawner = Arc::new(
            |_exe: &Path, _host: &str, _port: u16, _project: Option<&Path>, _allow_remote: bool| {
                unreachable!("spawner should not run")
            },
        );
        let resolve = Arc::new(|| Err("no binary".into()));
        let result = ensure_hub_impl(
            "127.0.0.1",
            19992,
            None,
            false,
            probe,
            spawner,
            resolve,
            Duration::from_millis(50),
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no binary"));
    }

    #[tokio::test]
    async fn run_hub_stop_stale_pid_while_hub_running() {
        use crate::hub::pid::{hub_pid_path, write_hub_pid};
        use crate::test_support::env_lock;

        let (base_url, _store) = crate::test_support::spawn_test_hub().await;
        let parts: Vec<&str> = base_url.trim_start_matches("http://").split(':').collect();
        let host = parts[0].to_string();
        let port: u16 = parts[1].parse().unwrap();

        let _guard = env_lock();
        let dir = std::env::temp_dir().join(format!("mizpah-hub-stale-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let old = std::env::var_os("MIZPAH_CONFIG_DIR");
        std::env::set_var("MIZPAH_CONFIG_DIR", &dir);
        write_hub_pid(port).unwrap();
        let pid_path = hub_pid_path(port).unwrap();
        std::fs::write(&pid_path, "999999999\n").unwrap();

        let result = run_hub_stop(host, port).await;

        match old {
            Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
            None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
        }
        let _ = std::fs::remove_dir_all(&dir);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("stale"));
    }
}
