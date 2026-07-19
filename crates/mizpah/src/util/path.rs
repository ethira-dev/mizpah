//! Executable / PATH lookup helpers.

use std::path::{Path, PathBuf};

pub fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Find `name` on `PATH` (first executable match).
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn is_executable_rejects_non_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_executable(&dir.path().join("missing")));
    }

    #[cfg(unix)]
    #[test]
    fn is_executable_requires_exec_bit() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tool");
        fs::write(&file, b"").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!is_executable(&file));
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_executable(&file));
    }

    #[test]
    fn which_finds_executable_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let name = format!("mizpah-test-bin-{}", std::process::id());
        let file = dir.path().join(&name);
        fs::write(&file, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();

        let new_path = format!(
            "{}:{}",
            dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        with_path(&new_path, || {
            let found = which(&name);
            assert_eq!(found.as_deref(), Some(file.as_path()));
        });
    }

    #[test]
    fn which_skips_non_executable() {
        let dir = tempfile::tempdir().unwrap();
        let name = format!("mizpah-noexec-{}", std::process::id());
        let file = dir.path().join(&name);
        fs::write(&file, b"").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();

        let new_path = format!(
            "{}:{}",
            dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        with_path(&new_path, || {
            assert!(which(&name).is_none());
        });
    }

    #[test]
    fn with_path_restores_when_path_was_unset() {
        let _guard = crate::test_support::env_lock();
        std::env::remove_var("PATH");
        with_path("/usr/bin:/bin", || {
            assert!(std::env::var_os("PATH").is_some());
        });
        assert!(std::env::var_os("PATH").is_none());
    }

    #[test]
    fn which_returns_none_without_path_env() {
        let _guard = crate::test_support::env_lock();
        std::env::remove_var("PATH");
        assert!(which("no-such-binary").is_none());
    }

    fn with_path<F, T>(path: &str, f: F) -> T
    where
        F: FnOnce() -> T,
    {
        let old = std::env::var_os("PATH");
        std::env::set_var("PATH", path);
        let out = f();
        match old {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
        out
    }
}
