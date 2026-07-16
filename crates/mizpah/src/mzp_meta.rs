//! Receiver identity attached to every ingested log row as `_mzp`.

use serde::{Deserialize, Serialize};

/// Mizpah receiver process metadata injected into each log payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MzpMeta {
    /// Working directory of the receiver stream (terminal cwd).
    pub cwd: String,
    /// OS user running the receiver process.
    pub user: String,
    /// PID of the Mizpah receiver process.
    pub pid: u32,
    /// Path of the Mizpah executable.
    pub exe: String,
}

impl MzpMeta {
    /// Capture identity for the current process.
    pub fn capture() -> Self {
        Self {
            cwd: cwd_string(),
            user: user_string(),
            pid: std::process::id(),
            exe: exe_string(),
        }
    }

    /// Override cwd (e.g. shell-attach control-frame directory).
    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = cwd.into();
        self
    }
}

fn cwd_string() -> String {
    match std::env::current_dir() {
        Ok(dir) => match dir.canonicalize() {
            Ok(canon) => canon.display().to_string(),
            Err(_) => dir.display().to_string(),
        },
        Err(_) => String::new(),
    }
}

fn user_string() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_default()
}

fn exe_string() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}
