//! Post-update hub restart helper.

use super::check::stable_exe_path;
use super::RestartContext;
use crate::unix_process;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

pub fn spawn_update_resume(ctx: &RestartContext) -> Result<(), String> {
    let exe = stable_exe_path().map_err(|e| e.to_string())?;
    let parent_pid = std::process::id();
    let mut cmd = Command::new(&exe);
    cmd.args([
        "update-resume",
        "--wait-pid",
        &parent_pid.to_string(),
        "--host",
        &ctx.host,
        "--port",
        &ctx.port.to_string(),
        "--max-bytes",
        &ctx.max_bytes.to_string(),
        "--ttl-hours",
        &ctx.ttl_hours.to_string(),
        "--project",
        &ctx.project_dir.to_string_lossy(),
    ]);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    unix_process::apply_pre_exec_setsid(&mut cmd);

    cmd.spawn()
        .map_err(|e| format!("spawn update-resume: {e}"))?;
    Ok(())
}

/// Hidden CLI: wait for parent exit + port free, then start detached hub.
pub async fn run_update_resume(
    wait_pid: u32,
    host: String,
    port: u16,
    project: PathBuf,
    max_bytes: u64,
    ttl_hours: u64,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        let parent_gone = !unix_process::process_exists(wait_pid);
        let port_free = !crate::hub::probe_hub(&host, port).await;
        if parent_gone && port_free {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    if crate::hub::probe_hub(&host, port).await {
        return Err(format!(
            "port {port} still in use after waiting for pid {wait_pid}"
        ));
    }

    let exe = stable_exe_path().map_err(|e| e.to_string())?;
    crate::hub::spawn_detached_hub_with_options(
        &exe,
        &host,
        port,
        Some(&project),
        Some(max_bytes),
        Some(ttl_hours),
        false,
    )
    .map_err(|e| format!("failed to start hub after update: {e}"))?;

    let ready_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < ready_deadline {
        if crate::hub::probe_hub(&host, port).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(format!(
        "hub at {host}:{port} did not become healthy after update",
    ))
}
