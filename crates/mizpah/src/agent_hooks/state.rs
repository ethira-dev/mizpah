//! Agent-hooks attach state persistence.

use crate::hub::{DEFAULT_HOST, DEFAULT_PORT};
use crate::util;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::PathBuf;

const STATE_FILE: &str = "agent-hooks.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookSource {
    Cursor,
    Claude,
}

impl HookSource {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cursor" => Some(Self::Cursor),
            "claude" => Some(Self::Claude),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cursor => "cursor",
            Self::Claude => "claude",
        }
    }

    pub(crate) fn default_service(self) -> &'static str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceState {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub service: String,
}

impl SourceState {
    pub(crate) fn disabled(source: HookSource) -> Self {
        Self {
            enabled: false,
            host: DEFAULT_HOST.to_string(),
            port: DEFAULT_PORT,
            service: source.default_service().to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentHooksState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<SourceState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<SourceState>,
}

fn state_path() -> io::Result<PathBuf> {
    Ok(util::config_dir()?.join(STATE_FILE))
}

pub(crate) fn load_state() -> io::Result<AgentHooksState> {
    load_state_from(&state_path()?)
}

pub(crate) fn load_state_from(path: &std::path::Path) -> io::Result<AgentHooksState> {
    match fs::read_to_string(path) {
        Ok(raw) if raw.trim().is_empty() => Ok(AgentHooksState::default()),
        Ok(raw) => {
            serde_json::from_str(&raw).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(AgentHooksState::default()),
        Err(e) => Err(e),
    }
}

pub(crate) fn save_state(state: &AgentHooksState) -> io::Result<()> {
    let dir = util::config_dir()?;
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    save_state_to(&state_path()?, state)
}

pub(crate) fn save_state_to(path: &std::path::Path, state: &AgentHooksState) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string_pretty(state)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    util::atomic_write(path, &raw)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_source_parse_and_labels() {
        assert_eq!(HookSource::parse("cursor"), Some(HookSource::Cursor));
        assert_eq!(HookSource::parse("CLAUDE"), Some(HookSource::Claude));
        assert_eq!(HookSource::parse("other"), None);
        assert_eq!(HookSource::Cursor.as_str(), "cursor");
        assert_eq!(HookSource::Claude.default_service(), "claude");
    }

    #[test]
    fn source_state_disabled_defaults() {
        let s = SourceState::disabled(HookSource::Cursor);
        assert!(!s.enabled);
        assert_eq!(s.service, "cursor");
    }

    #[test]
    fn save_load_roundtrip() {
        use crate::test_support::env_lock;
        let _guard = env_lock();
        let dir = std::env::temp_dir().join(format!("mizpah-agent-state-{}", std::process::id()));
        let path = dir.join("agent-hooks.json");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let state = AgentHooksState {
            cursor: Some(SourceState {
                enabled: true,
                host: "127.0.0.1".into(),
                port: 3149,
                service: "cursor".into(),
            }),
            claude: None,
        };
        save_state_to(&path, &state).unwrap();
        let loaded = load_state_from(&path).unwrap();
        assert_eq!(loaded, state);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_returns_default() {
        let dir = std::env::temp_dir().join(format!("mizpah-agent-miss-{}", std::process::id()));
        let path = dir.join("missing.json");
        let loaded = load_state_from(&path).unwrap();
        assert_eq!(loaded, AgentHooksState::default());
    }

    #[test]
    fn load_corrupt_json_errors() {
        let dir = std::env::temp_dir().join(format!("mizpah-agent-bad-{}", std::process::id()));
        let path = dir.join("agent-hooks.json");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, "{bad").unwrap();
        assert!(load_state_from(&path).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_load_via_config_dir() {
        use crate::test_support::env_lock;
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("MIZPAH_CONFIG_DIR", dir.path());
        let state = AgentHooksState {
            claude: Some(SourceState::disabled(HookSource::Claude)),
            cursor: None,
        };
        save_state(&state).unwrap();
        assert_eq!(load_state().unwrap(), state);
        std::env::remove_var("MIZPAH_CONFIG_DIR");
    }
}
