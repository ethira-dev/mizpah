//! Attach state persistence and config paths.

use crate::hub::{DEFAULT_HOST, DEFAULT_PORT};
use crate::util;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Zsh,
    Bash,
}

impl ShellKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "zsh" => Some(Self::Zsh),
            "bash" => Some(Self::Bash),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Zsh => "zsh",
            Self::Bash => "bash",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AttachState {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    pub host: String,
    pub port: u16,
}

impl Default for AttachState {
    fn default() -> Self {
        Self {
            enabled: false,
            service: None,
            host: DEFAULT_HOST.to_string(),
            port: DEFAULT_PORT,
        }
    }
}

pub fn state_path() -> io::Result<PathBuf> {
    Ok(util::config_dir()?.join("attach.json"))
}

/// Read attach state. Missing file → disabled defaults. Corrupted JSON → error.
pub fn load_state() -> io::Result<AttachState> {
    load_state_from(&state_path()?)
}

pub fn load_state_from(path: &Path) -> io::Result<AttachState> {
    match fs::read_to_string(path) {
        Ok(raw) => {
            if raw.trim().is_empty() {
                return Ok(AttachState::default());
            }
            serde_json::from_str(&raw).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("corrupt attach state {}: {e}", path.display()),
                )
            })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(AttachState::default()),
        Err(e) => Err(e),
    }
}

/// Atomically write attach state with user-only permissions.
pub fn save_state(state: &AttachState) -> io::Result<()> {
    let dir = util::config_dir()?;
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    save_state_to(&dir.join("attach.json"), state)
}

pub fn save_state_to(path: &Path, state: &AttachState) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let json = serde_json::to_vec_pretty(state)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&json)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_roundtrip_in_temp_config() {
        let s = AttachState {
            enabled: true,
            service: Some("api".into()),
            host: "127.0.0.1".into(),
            port: 3149,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: AttachState = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn save_load_state_via_path() {
        let dir = std::env::temp_dir().join(format!(
            "mizpah-attach-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("attach.json");

        let missing = load_state_from(&path).unwrap();
        assert!(!missing.enabled);

        let s = AttachState {
            enabled: true,
            service: Some("dev".into()),
            host: "127.0.0.1".into(),
            port: 3149,
        };
        save_state_to(&path, &s).unwrap();
        let loaded = load_state_from(&path).unwrap();
        assert_eq!(loaded, s);

        let mut disabled = loaded;
        disabled.enabled = false;
        save_state_to(&path, &disabled).unwrap();
        assert!(!load_state_from(&path).unwrap().enabled);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_state_errors() {
        let dir = std::env::temp_dir().join(format!(
            "mizpah-corrupt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("attach.json");
        fs::write(&path, "{not-json").unwrap();
        assert!(load_state_from(&path).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_empty_file_returns_default() {
        let dir = std::env::temp_dir().join(format!("mizpah-empty-attach-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("attach.json");
        fs::write(&path, "   \n").unwrap();
        assert_eq!(load_state_from(&path).unwrap(), AttachState::default());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn shell_kind_as_str() {
        assert_eq!(ShellKind::Zsh.as_str(), "zsh");
        assert_eq!(ShellKind::Bash.as_str(), "bash");
    }

    #[test]
    fn save_load_via_config_dir() {
        use crate::test_support::env_lock;
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("MIZPAH_CONFIG_DIR", dir.path());
        let state = AttachState {
            enabled: true,
            service: Some("svc".into()),
            host: "127.0.0.1".into(),
            port: 3149,
        };
        save_state(&state).unwrap();
        assert_eq!(load_state().unwrap(), state);
        std::env::remove_var("MIZPAH_CONFIG_DIR");
    }
}
