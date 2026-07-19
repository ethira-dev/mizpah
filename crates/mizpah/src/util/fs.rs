//! Home / config directory resolution and atomic file writes.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub fn home_dir() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("HOME") {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    // directories → libc getpwuid_r is unsupported under Miri.
    #[cfg(miri)]
    {
        return None;
    }
    #[cfg(not(miri))]
    {
        directories::UserDirs::new().map(|u| u.home_dir().to_path_buf())
    }
}

pub fn config_dir() -> io::Result<PathBuf> {
    if let Ok(dir) = std::env::var("MIZPAH_CONFIG_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    #[cfg(miri)]
    {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "could not resolve config dir under miri (set MIZPAH_CONFIG_DIR)",
        ));
    }
    #[cfg(not(miri))]
    {
        directories::ProjectDirs::from("dev", "ethira", "mizpah")
            .map(|d| d.config_dir().to_path_buf())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "could not resolve config dir"))
    }
}

/// Atomically replace `path` with `content`. On Unix, preserves existing mode when present,
/// otherwise uses `0o600`.
pub fn atomic_write(path: &Path, content: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path)
            .ok()
            .map_or(0o600, |m| m.permissions().mode());
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(mode));
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::env_lock;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn home_dir_returns_some() {
        assert!(home_dir().is_some());
    }

    #[test]
    fn home_dir_skips_empty_env() {
        let _guard = env_lock();
        let old = std::env::var_os("HOME");
        std::env::set_var("HOME", "   ");
        // Empty/whitespace HOME falls through; under miri there is no getpwuid fallback.
        #[cfg(not(miri))]
        assert!(home_dir().is_some());
        #[cfg(miri)]
        assert!(home_dir().is_none());
        match old {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn config_dir_honors_env_override() {
        let _guard = env_lock();
        let old = std::env::var_os("MIZPAH_CONFIG_DIR");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        std::env::set_var("MIZPAH_CONFIG_DIR", path);
        let resolved = config_dir().unwrap();
        assert_eq!(resolved, PathBuf::from(path));
        match old {
            Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
            None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
        }
    }

    #[test]
    fn config_dir_falls_back_when_env_empty() {
        let _guard = env_lock();
        let old = std::env::var_os("MIZPAH_CONFIG_DIR");
        std::env::set_var("MIZPAH_CONFIG_DIR", "   ");
        let resolved = config_dir();
        match old {
            Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
            None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
        }
        #[cfg(not(miri))]
        assert!(resolved.is_ok());
        #[cfg(miri)]
        assert!(resolved.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_create_and_overwrite_preserves_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg.toml");
        atomic_write(&path, "v1\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "v1\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        atomic_write(&path, "v2\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "v2\n");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
    }

    #[test]
    fn atomic_write_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/dir/file.txt");
        atomic_write(&path, "data").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "data");
    }
}
