//! Home / config directory resolution and atomic file writes.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub fn home_dir() -> Option<PathBuf> {
    directories::UserDirs::new().map(|u| u.home_dir().to_path_buf())
}

pub fn config_dir() -> io::Result<PathBuf> {
    if let Ok(dir) = std::env::var("MIZPAH_CONFIG_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    directories::ProjectDirs::from("dev", "ethira", "mizpah")
        .map(|d| d.config_dir().to_path_buf())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "could not resolve config dir"))
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
