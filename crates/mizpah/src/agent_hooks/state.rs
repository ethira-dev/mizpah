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
    let path = state_path()?;
    match fs::read_to_string(&path) {
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
    let path = state_path()?;
    let raw = serde_json::to_string_pretty(state)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    util::atomic_write(&path, &raw)?;
    Ok(())
}
