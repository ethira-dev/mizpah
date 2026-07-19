//! Post-update hub restart helper.

use super::check::stable_exe_path;
use super::RestartContext;
use crate::unix_process;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

pub type ProcessSpawner = Arc<dyn Fn(&mut Command) -> std::io::Result<Child> + Send + Sync>;

pub fn real_process_spawner() -> ProcessSpawner {
    Arc::new(|cmd: &mut Command| cmd.spawn())
}

pub fn spawn_update_resume(ctx: &RestartContext) -> Result<(), String> {
    spawn_update_resume_impl(ctx, real_process_spawner())
}

pub fn spawn_update_resume_impl(
    ctx: &RestartContext,
    spawner: ProcessSpawner,
) -> Result<(), String> {
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

    spawner(&mut cmd).map_err(|e| format!("spawn update-resume: {e}"))?;
    Ok(())
}

pub type HubProber = Arc<dyn Fn(&str, u16) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> + Send + Sync>;
pub type ProcessChecker = Arc<dyn Fn(u32) -> bool + Send + Sync>;
pub type HubSpawner = Arc<
    dyn Fn(&Path, &str, u16, Option<&Path>, Option<u64>, Option<u64>, bool) -> std::io::Result<Child>
        + Send
        + Sync,
>;

pub fn real_hub_prober() -> HubProber {
    Arc::new(|host: &str, port: u16| {
        let host = host.to_string();
        Box::pin(async move { crate::hub::probe_hub(&host, port).await })
    })
}

pub fn real_process_checker() -> ProcessChecker {
    Arc::new(|pid: u32| unix_process::process_exists(pid))
}

pub fn real_hub_spawner() -> HubSpawner {
    Arc::new(
        |exe: &Path,
         host: &str,
         port: u16,
         project: Option<&Path>,
         max_bytes: Option<u64>,
         ttl_hours: Option<u64>,
         allow_remote: bool| {
            crate::hub::spawn_detached_hub_with_options(
                exe,
                host,
                port,
                project,
                max_bytes,
                ttl_hours,
                allow_remote,
            )
        },
    )
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
    run_update_resume_impl(
        wait_pid,
        host,
        port,
        project,
        max_bytes,
        ttl_hours,
        real_hub_prober(),
        real_process_checker(),
        real_hub_spawner(),
    )
    .await
}

const WAIT_FOR_PARENT: Duration = Duration::from_secs(15);
const WAIT_FOR_HUB_READY: Duration = Duration::from_secs(10);

pub async fn run_update_resume_impl(
    wait_pid: u32,
    host: String,
    port: u16,
    project: PathBuf,
    max_bytes: u64,
    ttl_hours: u64,
    hub_prober: HubProber,
    process_checker: ProcessChecker,
    hub_spawner: HubSpawner,
) -> Result<(), String> {
    run_update_resume_impl_with_timeouts(
        wait_pid,
        host,
        port,
        project,
        max_bytes,
        ttl_hours,
        hub_prober,
        process_checker,
        hub_spawner,
        WAIT_FOR_PARENT,
        WAIT_FOR_HUB_READY,
    )
    .await
}

pub(crate) async fn run_update_resume_impl_with_timeouts(
    wait_pid: u32,
    host: String,
    port: u16,
    project: PathBuf,
    max_bytes: u64,
    ttl_hours: u64,
    hub_prober: HubProber,
    process_checker: ProcessChecker,
    hub_spawner: HubSpawner,
    wait_for_parent: Duration,
    wait_for_hub_ready: Duration,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + wait_for_parent;
    while tokio::time::Instant::now() < deadline {
        let parent_gone = !process_checker(wait_pid);
        let port_free = !hub_prober(&host, port).await;
        if parent_gone && port_free {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    if hub_prober(&host, port).await {
        return Err(format!(
            "port {port} still in use after waiting for pid {wait_pid}"
        ));
    }

    let exe = stable_exe_path().map_err(|e| e.to_string())?;
    hub_spawner(
        &exe,
        &host,
        port,
        Some(&project),
        Some(max_bytes),
        Some(ttl_hours),
        false,
    )
    .map_err(|e| format!("failed to start hub after update: {e}"))?;

    let ready_deadline = tokio::time::Instant::now() + wait_for_hub_ready;
    while tokio::time::Instant::now() < ready_deadline {
        if hub_prober(&host, port).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(format!(
        "hub at {host}:{port} did not become healthy after update",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn spawn_update_resume_with_mock_spawner() {
        let ctx = RestartContext {
            host: "127.0.0.1".into(),
            port: 3149,
            project_dir: PathBuf::from("/tmp/test"),
            max_bytes: 1024,
            ttl_hours: 1,
        };

        let spawned = Arc::new(AtomicBool::new(false));
        let spawned_clone = Arc::clone(&spawned);
        let mock_spawner = Arc::new(move |_cmd: &mut Command| -> std::io::Result<Child> {
            spawned_clone.store(true, Ordering::SeqCst);
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "mock spawn",
            ))
        });

        let result = spawn_update_resume_impl(&ctx, mock_spawner);
        assert!(result.is_err());
        assert!(spawned.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn run_update_resume_parent_already_gone() {
        let hub_prober = Arc::new(|_host: &str, _port: u16| {
            Box::pin(async { false }) as std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        });
        let process_checker = Arc::new(|_pid: u32| false);
        let hub_spawner = Arc::new(
            |_exe: &Path,
             _host: &str,
             _port: u16,
             _project: Option<&Path>,
             _max_bytes: Option<u64>,
             _ttl_hours: Option<u64>,
             _allow_remote: bool|
             -> std::io::Result<Child> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "spawn failed",
                ))
            },
        );

        let result = run_update_resume_impl(
            99999,
            "127.0.0.1".into(),
            3149,
            PathBuf::from("/tmp"),
            1024,
            1,
            hub_prober,
            process_checker,
            hub_spawner,
        )
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("failed to start hub"));
    }

    #[tokio::test]
    async fn run_update_resume_port_still_in_use() {
        let hub_prober = Arc::new(|_host: &str, _port: u16| {
            Box::pin(async { true }) as std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        });
        let process_checker = Arc::new(|_pid: u32| false);
        let hub_spawner = Arc::new(
            |_exe: &Path,
             _host: &str,
             _port: u16,
             _project: Option<&Path>,
             _max_bytes: Option<u64>,
             _ttl_hours: Option<u64>,
             _allow_remote: bool|
             -> std::io::Result<Child> { unreachable!() },
        );

        let result = run_update_resume_impl(
            99999,
            "127.0.0.1".into(),
            3149,
            PathBuf::from("/tmp"),
            1024,
            1,
            hub_prober,
            process_checker,
            hub_spawner,
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("port"));
        assert!(err.contains("still in use"));
    }

    #[tokio::test]
    async fn run_update_resume_hub_never_becomes_healthy() {
        let hub_prober = Arc::new(|_host: &str, _port: u16| {
            Box::pin(async { false }) as std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        });
        let process_checker = Arc::new(|_pid: u32| false);
        
        #[cfg(unix)]
        fn spawn_mock_child() -> Child {
            use std::process::Stdio;
            let true_bin = ["/usr/bin/true", "/bin/true"]
                .into_iter()
                .find(|p| std::path::Path::new(p).is_file())
                .expect("true binary");
            Command::new(true_bin)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        }
        
        #[cfg(not(unix))]
        fn spawn_mock_child() -> Child {
            panic!("test only runs on unix")
        }
        
        let hub_spawner = Arc::new(
            |_exe: &Path,
             _host: &str,
             _port: u16,
             _project: Option<&Path>,
             _max_bytes: Option<u64>,
             _ttl_hours: Option<u64>,
             _allow_remote: bool|
             -> std::io::Result<Child> { Ok(spawn_mock_child()) },
        );

        let result = run_update_resume_impl(
            99999,
            "127.0.0.1".into(),
            3149,
            PathBuf::from("/tmp"),
            1024,
            1,
            hub_prober,
            process_checker,
            hub_spawner,
        )
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("did not become healthy"));
    }

    #[tokio::test]
    async fn run_update_resume_success() {
        let call_count = Arc::new(std::sync::Mutex::new(0));
        let call_count_clone = Arc::clone(&call_count);
        
        // Returns false while waiting (port free), true only after hub spawn
        // has been attempted (3rd+ probe: wait-loop, pre-spawn check, then ready).
        let hub_prober = Arc::new(move |_host: &str, _port: u16| {
            let count_clone = Arc::clone(&call_count_clone);
            Box::pin(async move {
                let mut count = count_clone.lock().unwrap();
                *count += 1;
                *count >= 3
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        });
        
        let process_checker = Arc::new(|_pid: u32| false);
        
        #[cfg(unix)]
        fn spawn_mock_child() -> Child {
            use std::process::Stdio;
            let true_bin = ["/usr/bin/true", "/bin/true"]
                .into_iter()
                .find(|p| std::path::Path::new(p).is_file())
                .expect("true binary");
            Command::new(true_bin)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        }
        
        #[cfg(not(unix))]
        fn spawn_mock_child() -> Child {
            panic!("test only runs on unix")
        }
        
        let hub_spawner = Arc::new(
            |_exe: &Path,
             _host: &str,
             _port: u16,
             _project: Option<&Path>,
             _max_bytes: Option<u64>,
             _ttl_hours: Option<u64>,
             _allow_remote: bool|
             -> std::io::Result<Child> { Ok(spawn_mock_child()) },
        );

        let result = run_update_resume_impl(
            99999,
            "127.0.0.1".into(),
            3149,
            PathBuf::from("/tmp"),
            1024,
            1,
            hub_prober,
            process_checker,
            hub_spawner,
        )
        .await;

        assert!(result.is_ok());
    }

    #[test]
    fn real_injectors_construct() {
        let _ = real_process_spawner();
        let _ = real_hub_prober();
        let _ = real_process_checker();
        let _ = real_hub_spawner();
    }

    #[test]
    fn spawn_update_resume_success_with_mock_spawner() {
        let ctx = RestartContext {
            host: "127.0.0.1".into(),
            port: 3149,
            project_dir: PathBuf::from("/tmp/test"),
            max_bytes: 1024,
            ttl_hours: 1,
        };
        #[cfg(unix)]
        fn mock_child() -> Child {
            use std::process::{Command, Stdio};
            Command::new("/bin/sleep")
                .arg("1")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        }
        #[cfg(not(unix))]
        fn mock_child() -> Child {
            panic!("test only runs on unix")
        }
        let mock_spawner = Arc::new(|_cmd: &mut Command| Ok(mock_child()));
        assert!(spawn_update_resume_impl(&ctx, mock_spawner).is_ok());
    }

    #[tokio::test]
    async fn run_update_resume_waits_for_parent() {
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let alive_clone = Arc::clone(&alive);
        let hub_prober = Arc::new(|_host: &str, _port: u16| {
            Box::pin(async { false }) as std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        });
        let process_checker = Arc::new(move |_pid: u32| alive_clone.load(std::sync::atomic::Ordering::SeqCst));
        #[cfg(unix)]
        fn mock_child() -> Child {
            use std::process::{Command, Stdio};
            Command::new("/bin/sleep")
                .arg("1")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        }
        #[cfg(not(unix))]
        fn mock_child() -> Child {
            panic!("test only runs on unix")
        }
        let hub_spawner = Arc::new(
            |_exe: &Path,
             _host: &str,
             _port: u16,
             _project: Option<&Path>,
             _max_bytes: Option<u64>,
             _ttl_hours: Option<u64>,
             _allow_remote: bool|
             -> std::io::Result<Child> { Ok(mock_child()) },
        );
        let alive_for_timer = Arc::clone(&alive);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(80)).await;
            alive_for_timer.store(false, std::sync::atomic::Ordering::SeqCst);
        });
        let result = run_update_resume_impl_with_timeouts(
            99999,
            "127.0.0.1".into(),
            3149,
            PathBuf::from("/tmp"),
            1024,
            1,
            hub_prober,
            process_checker,
            hub_spawner,
            Duration::from_millis(300),
            Duration::from_millis(50),
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("did not become healthy"));
    }
}
