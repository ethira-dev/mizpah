//! Cross-platform process helpers (existence checks, signals, session detach).

use std::io;
#[cfg(unix)]
use std::process::Command;
#[cfg(windows)]
use std::process::{Command, Stdio};

/// Best-effort process hardening for the hub: disable core dumps and (on Linux)
/// mark the process non-dumpable so casual same-user `ptrace` attach fails.
///
/// Does not stop root / `CAP_SYS_PTRACE` or a determined same-user attacker who
/// replaces the binary. Safe to call multiple times.
pub fn harden_process() {
    #[cfg(unix)]
    {
        // SAFETY: setrlimit with RLIMIT_CORE is a standard hardening call.
        let lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        let _ = unsafe { libc::setrlimit(libc::RLIMIT_CORE, &lim) };

        #[cfg(target_os = "linux")]
        {
            // SAFETY: prctl(PR_SET_DUMPABLE) has well-defined args.
            let _ = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
        }
    }
}

/// Return whether `pid` refers to a live process (or one we may signal).
pub fn process_exists(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0) checks existence / permission without signaling.
        // SAFETY: libc::kill with signal 0 is a standard existence probe.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(windows)]
    {
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .map(|o| {
                let s = String::from_utf8_lossy(&o.stdout);
                s.contains(&pid.to_string())
            })
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

/// Send SIGTERM (Unix) or taskkill (Windows) to `pid`.
pub fn signal_term(pid: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        // SAFETY: sending SIGTERM to a PID we own via hub.pid.
        let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if rc == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        Err(format!("failed to SIGTERM pid {pid}: {err}"))
    }
    #[cfg(windows)]
    {
        // Soft stop is not reliable on Windows; terminate immediately.
        signal_kill(pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(format!("cannot signal pid {pid} on this platform"))
    }
}

/// Send SIGKILL (Unix) or forced taskkill (Windows) to `pid`.
pub fn signal_kill(pid: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        // SAFETY: sending SIGKILL after stop timeout.
        let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        if rc == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        Err(format!("failed to SIGKILL pid {pid}: {err}"))
    }
    #[cfg(windows)]
    {
        let status = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F", "/T"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("failed to taskkill pid {pid}: {e}"))?;
        if status.success() {
            Ok(())
        } else if status.code() == Some(128) {
            // Exit code 128 = process not found
            Ok(())
        } else {
            Err(format!("taskkill pid {pid} failed with {status}"))
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(format!("cannot kill pid {pid} on this platform"))
    }
}

#[cfg(unix)]
fn pre_exec_setsid() -> io::Result<()> {
    // SAFETY: setsid(2) creates a new session; standard for detaching child processes.
    if unsafe { libc::setsid() } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Call `setsid` in a [`Command`](std::process::Command) pre_exec hook (Unix only).
#[cfg(unix)]
pub fn apply_pre_exec_setsid(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: pre_exec runs in the child between fork and exec; setsid is the intended use.
    unsafe {
        cmd.pre_exec(pre_exec_setsid);
    }
}

#[cfg(all(test, not(miri)))]
mod ffi_tests {
    use super::*;

    #[test]
    fn harden_process_is_idempotent() {
        harden_process();
        harden_process();
    }

    #[test]
    fn process_exists_current_pid() {
        assert!(process_exists(std::process::id()));
    }

    #[test]
    fn process_exists_nonexistent_pid() {
        // Avoid u32::MAX — casts to -1 / kill(-1) on unix.
        assert!(!process_exists(999_999_999));
    }

    #[test]
    fn signal_term_and_kill_ok_for_missing_pid() {
        assert!(signal_term(999_999_999).is_ok());
        assert!(signal_kill(999_999_999).is_ok());
    }

    #[test]
    fn apply_pre_exec_setsid_runs_child() {
        use std::process::Command;
        let true_bin = ["/usr/bin/true", "/bin/true"]
            .into_iter()
            .find(|p| std::path::Path::new(p).is_file())
            .expect("true binary");
        let mut cmd = Command::new(true_bin);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        apply_pre_exec_setsid(&mut cmd);
        let status = cmd.status().unwrap();
        assert!(status.success());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pre_exec_setsid_is_unix_only_api() {
        // Pure compile-time / cfg guard: non-unix builds must not expose apply_pre_exec_setsid.
        #[cfg(unix)]
        {
            let _ = super::pre_exec_setsid as fn() -> std::io::Result<()>;
        }
    }
}
