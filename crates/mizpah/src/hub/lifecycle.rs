//! Hub health checks, lifecycle, and detached spawn.

use crate::error::HubLifecycleError;
use crate::hub::pid::{read_hub_pid, remove_hub_pid};
use crate::unix_process;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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
    if probe_hub(host, port).await {
        return Ok(());
    }

    if !is_loopback_host(host) && !allow_remote {
        return Err(HubLifecycleError::msg(format!(
            "hub at {} is not reachable; start it remotely, use a loopback host, or pass --allow-remote",
            hub_url(host, port)
        )));
    }

    let exe = crate::mcp::resolve_binary_path()
        .map_err(|e| HubLifecycleError::msg(format!("could not resolve mizpah binary: {e}")))?;

    let project_buf = project
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::current_dir().ok());
    let mut child = spawn_detached_hub(&exe, host, port, project_buf.as_deref(), allow_remote)
        .map_err(|e| HubLifecycleError::msg(format!("failed to start hub: {e}")))?;

    let deadline = tokio::time::Instant::now() + HUB_STARTUP_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if probe_hub(host, port).await {
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
        HUB_STARTUP_TIMEOUT.as_secs()
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
    spawn_detached_hub_with_options(exe, host, port, project, None, allow_remote)
}

/// Spawn a detached hub process. Optionally pass `--max-bytes` (used after self-update).
pub fn spawn_detached_hub_with_options(
    exe: &Path,
    host: &str,
    port: u16,
    project: Option<&Path>,
    max_bytes: Option<u64>,
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
}
