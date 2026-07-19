//! Shared command markers and config file I/O for agent hooks.

use super::state::HookSource;
use crate::util;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub(crate) const HOOK_MARKER: &str = "__hook-forward --source ";

pub(crate) fn managed_command(bin: &Path, source: HookSource) -> String {
    format!(
        "{} {}{}",
        util::shell_quote_path(bin),
        HOOK_MARKER,
        source.as_str()
    )
}

pub(crate) fn is_managed_command(command: &str, source: HookSource) -> bool {
    let needle = format!("{HOOK_MARKER}{}", source.as_str());
    command.contains(&needle)
}

pub(crate) fn read_file_or_empty(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e),
    }
}

pub(crate) fn write_config_file(path: &Path, content: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    util::atomic_write(path, content)
}

pub(crate) fn cursor_hooks_path() -> Option<PathBuf> {
    Some(util::home_dir()?.join(".cursor").join("hooks.json"))
}

pub(crate) fn claude_settings_path() -> Option<PathBuf> {
    Some(util::home_dir()?.join(".claude").join("settings.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hooks::state::HookSource;
    use crate::test_support::env_lock;

    #[test]
    fn read_file_or_empty_missing_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.json");
        assert_eq!(read_file_or_empty(&path).unwrap(), "");
    }

    #[test]
    fn write_config_file_creates_nested_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/settings.json");
        write_config_file(&path, "{}\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{}\n");
    }

    #[test]
    fn managed_command_markers_are_source_specific() {
        let cmd = managed_command(Path::new("/bin/mzp"), HookSource::Claude);
        assert!(is_managed_command(&cmd, HookSource::Claude));
        assert!(!is_managed_command(&cmd, HookSource::Cursor));
    }

    #[test]
    fn config_paths_under_home() {
        let _guard = env_lock();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        assert_eq!(
            cursor_hooks_path().unwrap(),
            home.path().join(".cursor/hooks.json")
        );
        assert_eq!(
            claude_settings_path().unwrap(),
            home.path().join(".claude/settings.json")
        );
        std::env::remove_var("HOME");
    }
}
