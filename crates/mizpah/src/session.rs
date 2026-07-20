//! Persist UI filter session JSON under the Mizpah config directory.

use crate::util::{atomic_write, config_dir};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Sticky UI filter / time-range / service selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UiSession {
    pub q: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
}

/// Path to the last-used UI session file (`<config>/session/last.json`).
pub fn session_path() -> PathBuf {
    config_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("session")
        .join("last.json")
}

fn sessions_dir() -> Result<PathBuf, String> {
    let dir = config_dir().map_err(|e| e.to_string())?.join("sessions");
    fs::create_dir_all(&dir).map_err(|e| format!("create sessions dir: {e}"))?;
    Ok(dir)
}

#[allow(dead_code)]
fn named_path(name: &str) -> Result<PathBuf, String> {
    let safe = sanitize_session_name(name)?;
    Ok(sessions_dir()?.join(format!("{safe}.json")))
}

#[allow(dead_code)]
fn sanitize_session_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("session name must not be empty".into());
    }
    if trimmed.len() > 64 {
        return Err("session name too long (max 64)".into());
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(
            "session name may only contain alphanumeric, '_', '-', or '.' characters".into(),
        );
    }
    Ok(trimmed.to_string())
}

/// Load the last-used UI session, if present and valid.
pub fn load_last() -> Option<UiSession> {
    let path = session_path();
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Persist the last-used UI session.
pub fn save_last(session: &UiSession) -> Result<(), String> {
    let path = session_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create session dir: {e}"))?;
    }
    let json = serde_json::to_string_pretty(session).map_err(|e| e.to_string())?;
    atomic_write(&path, &format!("{json}\n")).map_err(|e| format!("write session: {e}"))
}

/// Save a named session under `<config>/sessions/<name>.json`.
#[allow(dead_code)]
pub fn save_named(name: &str, session: &UiSession) -> Result<(), String> {
    let path = named_path(name)?;
    let json = serde_json::to_string_pretty(session).map_err(|e| e.to_string())?;
    atomic_write(&path, &format!("{json}\n")).map_err(|e| format!("write named session: {e}"))
}

/// Load a named session from `<config>/sessions/<name>.json`.
#[allow(dead_code)]
pub fn load_named(name: &str) -> Option<UiSession> {
    let path = named_path(name).ok()?;
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::env_lock;
    use tempfile::TempDir;

    fn with_temp_config<F: FnOnce(&std::path::Path)>(f: F) {
        let _guard = env_lock();
        let dir = TempDir::new().unwrap();
        let old = std::env::var_os("MIZPAH_CONFIG_DIR");
        std::env::set_var("MIZPAH_CONFIG_DIR", dir.path());
        f(dir.path());
        match old {
            Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
            None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
        }
    }

    #[test]
    fn session_path_under_config_dir() {
        with_temp_config(|dir| {
            let path = session_path();
            assert_eq!(path, dir.join("session").join("last.json"));
        });
    }

    #[test]
    fn save_and_load_last() {
        with_temp_config(|_| {
            assert!(load_last().is_none());
            let s = UiSession {
                q: r#"level == "error""#.into(),
                from: Some("2026-01-01T00:00:00Z".into()),
                to: None,
                service: Some("api".into()),
            };
            save_last(&s).unwrap();
            assert_eq!(load_last(), Some(s));
        });
    }

    #[test]
    fn named_sessions_roundtrip() {
        with_temp_config(|dir| {
            let s = UiSession {
                q: "has(user.id)".into(),
                from: None,
                to: None,
                service: None,
            };
            save_named("incident-a", &s).unwrap();
            assert_eq!(load_named("incident-a"), Some(s));
            assert!(dir.join("sessions").join("incident-a.json").is_file());
            assert!(load_named("missing").is_none());
        });
    }

    #[test]
    fn rejects_bad_session_names() {
        with_temp_config(|_| {
            let s = UiSession::default();
            assert!(save_named("../evil", &s).is_err());
            assert!(save_named("", &s).is_err());
            assert!(save_named("has spaces", &s).is_err());
        });
    }
}
