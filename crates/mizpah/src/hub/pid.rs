//! Hub PID file under the Mizpah config directory.

use crate::util;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

pub fn hub_pid_path(port: u16) -> io::Result<PathBuf> {
    Ok(util::config_dir()?.join(format!("hub-{port}.pid")))
}

/// Best-effort: write this process's PID for `mzp hub stop`.
pub fn write_hub_pid(port: u16) -> io::Result<()> {
    let dir = util::config_dir()?;
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    let path = hub_pid_path(port)?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut f = fs::File::create(&tmp)?;
        writeln!(f, "{}", std::process::id())?;
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

pub fn read_hub_pid(port: u16) -> io::Result<Option<u32>> {
    let path = hub_pid_path(port)?;
    match fs::read_to_string(&path) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            let pid = trimmed.parse::<u32>().map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("corrupt hub PID file {}: {e}", path.display()),
                )
            })?;
            Ok(Some(pid))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn remove_hub_pid(port: u16) -> io::Result<()> {
    let path = hub_pid_path(port)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::env_lock;
    use crate::unix_process;

    fn with_isolated_config_dir<F: FnOnce(&std::path::Path)>(suffix: &str, f: F) {
        let _guard = env_lock();
        let dir = std::env::temp_dir().join(format!(
            "mizpah-hub-pid-{suffix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let old = std::env::var_os("MIZPAH_CONFIG_DIR");
        std::env::set_var("MIZPAH_CONFIG_DIR", &dir);
        f(&dir);
        match old {
            Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
            None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(not(miri))]
    fn hub_pid_roundtrip_and_stale_cleanup() {
        with_isolated_config_dir("roundtrip", |dir| {
            let port = 3149u16;
            assert!(read_hub_pid(port).unwrap().is_none());

            write_hub_pid(port).unwrap();
            assert_eq!(read_hub_pid(port).unwrap(), Some(std::process::id()));
            assert!(hub_pid_path(port).unwrap().starts_with(dir));

            remove_hub_pid(port).unwrap();
            assert!(read_hub_pid(port).unwrap().is_none());

            fs::write(hub_pid_path(port).unwrap(), "not-a-pid\n").unwrap();
            assert!(read_hub_pid(port).is_err());
            remove_hub_pid(port).unwrap();

            assert!(!unix_process::process_exists(999_999_999));
        });
    }

    #[test]
    #[cfg(not(miri))]
    fn empty_pid_file_returns_none() {
        with_isolated_config_dir("empty", |dir| {
            let port = 3150u16;
            fs::write(hub_pid_path(port).unwrap(), "\n").unwrap();
            assert!(read_hub_pid(port).unwrap().is_none());
            let _ = dir;
        });
    }

    #[test]
    #[cfg(not(miri))]
    fn remove_missing_hub_pid_is_ok() {
        with_isolated_config_dir("remove-missing", |_| {
            remove_hub_pid(3999).unwrap();
        });
    }

    #[test]
    #[cfg(not(miri))]
    fn read_hub_pid_propagates_io_errors() {
        with_isolated_config_dir("io", |dir| {
            let port = 3151u16;
            let path = hub_pid_path(port).unwrap();
            fs::write(&path, "12345\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
                assert!(read_hub_pid(port).is_err());
            }
            let _ = dir;
        });
    }
}
