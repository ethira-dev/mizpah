//! Homebrew and direct binary update application.

use super::check::{
    fetch_latest_release, find_brew_binary, parse_cli_version, release_target, running_bin_name,
    sibling_bin_name, stable_exe_path,
};
use super::{
    ProgressTx, UpdateChannel, UpdateEvent, UpdateManager, BREW_FORMULA, DOWNLOAD_TIMEOUT,
};
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use semver::Version;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;
use tar::Archive;
use tracing::warn;

pub async fn apply_update(manager: Arc<UpdateManager>, latest: Version, tx: ProgressTx) {
    let channel = {
        let g = manager.inner.lock().await;
        g.channel
    };

    let result = match channel {
        UpdateChannel::Homebrew => apply_homebrew(&latest, &tx).await,
        UpdateChannel::Direct => apply_direct(&latest, &tx).await,
    };

    match result {
        Ok(()) => {
            let _ = tx.send(UpdateEvent {
                step: "Restarting Mizpah…".into(),
                progress: 0.95,
                error: None,
                restarting: Some(true),
            });
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Err(err) = super::resume::spawn_update_resume(manager.restart_context()) {
                warn!(error = %err, "failed to spawn update-resume helper");
                let _ = tx.send(UpdateEvent {
                    step: "Restart failed".into(),
                    progress: 0.95,
                    error: Some(err),
                    restarting: None,
                });
                manager.clear_busy().await;
                return;
            }
            std::process::exit(0);
        }
        Err(err) => {
            let _ = tx.send(UpdateEvent {
                step: "Update failed".into(),
                progress: 0.0,
                error: Some(err),
                restarting: None,
            });
            manager.clear_busy().await;
        }
    }
}

fn emit(tx: &ProgressTx, step: impl Into<String>, progress: f32) {
    let _ = tx.send(UpdateEvent {
        step: step.into(),
        progress,
        error: None,
        restarting: None,
    });
}

async fn apply_homebrew(latest: &Version, tx: &ProgressTx) -> Result<(), String> {
    emit(tx, "Checking Homebrew…", 0.1);
    let brew = find_brew_binary()
        .ok_or_else(|| "Homebrew install detected but `brew` was not found".to_string())?;

    emit(tx, "Running brew upgrade…", 0.35);
    let output = tokio::task::spawn_blocking({
        let brew = brew.clone();
        move || {
            Command::new(&brew)
                .args(["upgrade", BREW_FORMULA])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
        }
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("failed to run brew: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = stderr.trim();
        if msg.is_empty() {
            return Err(format!("brew upgrade failed ({})", output.status));
        }
        return Err(truncate_err(msg));
    }

    emit(tx, "Verifying installed version…", 0.75);
    let stable = stable_exe_path().map_err(|e| e.to_string())?;
    let ver_out = tokio::task::spawn_blocking({
        let stable = stable.clone();
        move || {
            Command::new(&stable)
                .arg("--version")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
        }
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("failed to run --version on {}: {e}", stable.display()))?;

    let stdout = String::from_utf8_lossy(&ver_out.stdout);
    let installed = parse_cli_version(&stdout)
        .ok_or_else(|| format!("could not parse version from: {}", stdout.trim()))?;

    if installed < *latest {
        return Err(format!(
            "Homebrew formula is still {installed}; GitHub has {latest}. \
             The tap may not be updated yet — try again later."
        ));
    }
    Ok(())
}

async fn apply_direct(latest: &Version, tx: &ProgressTx) -> Result<(), String> {
    emit(tx, "Checking latest release…", 0.05);
    let target = release_target().ok_or_else(|| {
        "No prebuilt binary for this platform. Install via Homebrew or build from source."
            .to_string()
    })?;

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let install_dir = exe
        .parent()
        .ok_or_else(|| "could not determine install directory".to_string())?
        .to_path_buf();

    preflight_writable(&install_dir)?;

    let info = fetch_latest_release().await?;
    if info.version < *latest {
        // Shouldn't happen; continue with fetched.
    }
    let url = info.download_url.ok_or_else(|| {
        format!(
            "Release v{} has no asset mizpah-{target}.tar.gz",
            info.version
        )
    })?;

    emit(tx, "Downloading update…", 0.15);
    let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let archive_path = tmp.path().join(format!("mizpah-{target}.tar.gz"));
    download_with_progress(&url, &archive_path, tx).await?;

    emit(tx, "Installing binaries…", 0.75);
    let extract_dir = tmp.path().join("extract");
    fs::create_dir_all(&extract_dir).map_err(|e| e.to_string())?;
    extract_tarball(&archive_path, &extract_dir)?;

    let new_mizpah = extract_dir.join("mizpah");
    let new_mzp = extract_dir.join("mzp");
    if !new_mizpah.is_file() || !new_mzp.is_file() {
        return Err("archive missing mizpah or mzp binary".into());
    }
    set_executable(&new_mizpah)?;
    set_executable(&new_mzp)?;
    #[cfg(target_os = "macos")]
    {
        clear_quarantine(&new_mizpah);
        clear_quarantine(&new_mzp);
    }

    let running_name = running_bin_name(&exe);
    let sibling_name = sibling_bin_name(&running_name);
    let sibling_dest = install_dir.join(sibling_name);
    let new_running = if running_name == "mzp" {
        &new_mzp
    } else {
        &new_mizpah
    };
    let new_sibling = if sibling_name == "mzp" {
        &new_mzp
    } else {
        &new_mizpah
    };

    let same_inode = same_file(&exe, &sibling_dest).unwrap_or(false);
    if same_inode || !sibling_dest.exists() {
        self_replace::self_replace(new_running).map_err(|e| e.to_string())?;
        if !same_inode && sibling_name != running_name.as_str() {
            atomic_replace_file(new_sibling, &sibling_dest)?;
        }
    } else {
        atomic_replace_file(new_sibling, &sibling_dest)?;
        self_replace::self_replace(new_running).map_err(|e| e.to_string())?;
    }

    let _ = latest;
    let _ = info;
    Ok(())
}

fn preflight_writable(dir: &Path) -> Result<(), String> {
    let probe = dir.join(format!(".mizpah-write-test-{}", std::process::id()));
    match File::create(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => Err(format!(
            "install directory {} is not writable: {e}",
            dir.display()
        )),
    }
}

async fn download_with_progress(url: &str, dest: &Path, tx: &ProgressTx) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .user_agent(format!("mizpah/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;

    let resp = client
        .get(url)
        .header("Accept", "application/octet-stream")
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;

    let total = resp.content_length().unwrap_or(0);
    let mut file = File::create(dest).map_err(|e| e.to_string())?;
    let mut stream = resp.bytes_stream();
    let mut written: u64 = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        written += chunk.len() as u64;
        let frac = if total > 0 {
            0.15 + 0.55 * (written as f32 / total as f32)
        } else {
            0.4
        };
        emit(tx, "Downloading update…", frac.clamp(0.15, 0.7));
    }
    file.sync_all().map_err(|e| e.to_string())?;
    Ok(())
}

fn extract_tarball(archive: &Path, dest: &Path) -> Result<(), String> {
    let file = File::open(archive).map_err(|e| e.to_string())?;
    let dec = GzDecoder::new(file);
    let mut tar = Archive::new(dec);
    tar.unpack(dest).map_err(|e| e.to_string())?;
    Ok(())
}

fn set_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path).map_err(|e| e.to_string())?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).map_err(|e| e.to_string())?;
    }
    let _ = path;
    Ok(())
}

#[cfg(target_os = "macos")]
fn clear_quarantine(path: &Path) {
    let _ = Command::new("xattr")
        .args(["-d", "com.apple.quarantine"])
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn atomic_replace_file(src: &Path, dest: &Path) -> Result<(), String> {
    let parent = dest
        .parent()
        .ok_or_else(|| "invalid destination".to_string())?;
    let tmp = parent.join(format!(
        ".{}.new.{}",
        dest.file_name().and_then(|s| s.to_str()).unwrap_or("bin"),
        std::process::id()
    ));
    fs::copy(src, &tmp).map_err(|e| e.to_string())?;
    set_executable(&tmp)?;
    #[cfg(target_os = "macos")]
    clear_quarantine(&tmp);
    fs::rename(&tmp, dest).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.to_string()
    })?;
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> io::Result<bool> {
    if !a.exists() || !b.exists() {
        return Ok(false);
    }
    let ma = fs::metadata(a)?;
    let mb = fs::metadata(b)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(ma.dev() == mb.dev() && ma.ino() == mb.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = (ma, mb);
        Ok(fs::canonicalize(a)? == fs::canonicalize(b)?)
    }
}

fn truncate_err(msg: &str) -> String {
    const MAX: usize = 400;
    if msg.len() <= MAX {
        msg.to_string()
    } else {
        format!("{}…", &msg[..MAX])
    }
}
