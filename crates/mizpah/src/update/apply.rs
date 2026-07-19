//! Homebrew and direct binary update application.

use super::check::{
    fetch_latest_release, find_brew_binary, parse_cli_version, release_target, running_bin_name,
    sibling_bin_name, stable_exe_path, ReleaseInfo,
};
use super::{
    ProgressTx, UpdateChannel, UpdateEvent, UpdateManager, BREW_FORMULA, DOWNLOAD_TIMEOUT,
};
use crate::store::Store;
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use semver::Version;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::Duration;
use tar::Archive;
use tracing::warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    RestartRequested,
    Failed,
}

pub type BrewUpgradeFn = Arc<dyn Fn(&Path) -> io::Result<Output> + Send + Sync>;
pub type VersionCheckFn = Arc<dyn Fn(&Path) -> io::Result<Output> + Send + Sync>;
pub type SelfReplaceFn = Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;
pub type ReleaseFetchFn = Arc<
    dyn Fn() -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<ReleaseInfo, String>> + Send>,
        > + Send
        + Sync,
>;
pub type FindBrewFn = Arc<dyn Fn() -> Option<std::path::PathBuf> + Send + Sync>;
pub type StableExeFn = Arc<dyn Fn() -> io::Result<std::path::PathBuf> + Send + Sync>;
pub type ReleaseTargetFn = Arc<dyn Fn() -> Option<String> + Send + Sync>;
pub type CurrentExeFn = Arc<dyn Fn() -> io::Result<std::path::PathBuf> + Send + Sync>;
pub type DownloadFn = Arc<
    dyn Fn(
            String,
            std::path::PathBuf,
            ProgressTx,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

pub fn real_brew_upgrade() -> BrewUpgradeFn {
    Arc::new(|brew: &Path| {
        Command::new(brew)
            .args(["upgrade", BREW_FORMULA])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
    })
}

pub fn real_version_check() -> VersionCheckFn {
    Arc::new(|exe: &Path| {
        Command::new(exe)
            .arg("--version")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
    })
}

pub fn real_self_replace() -> SelfReplaceFn {
    Arc::new(|new_exe: &Path| self_replace::self_replace(new_exe).map_err(|e| e.to_string()))
}

pub fn real_release_fetch() -> ReleaseFetchFn {
    Arc::new(|| Box::pin(fetch_latest_release()) as _)
}

pub fn real_find_brew() -> FindBrewFn {
    Arc::new(find_brew_binary)
}

pub fn real_stable_exe() -> StableExeFn {
    Arc::new(stable_exe_path)
}

pub fn real_release_target() -> ReleaseTargetFn {
    Arc::new(|| release_target().map(str::to_string))
}

pub fn real_current_exe() -> CurrentExeFn {
    Arc::new(std::env::current_exe)
}

pub fn real_download() -> DownloadFn {
    Arc::new(|url, dest, tx| {
        Box::pin(async move { download_with_progress(&url, &dest, &tx).await }) as _
    })
}

pub async fn apply_update(
    manager: Arc<UpdateManager>,
    store: Arc<Store>,
    latest: Version,
    tx: ProgressTx,
) -> ApplyOutcome {
    apply_update_impl(
        manager,
        store,
        latest,
        tx,
        real_brew_upgrade(),
        real_version_check(),
        real_self_replace(),
        real_release_fetch(),
        real_find_brew(),
        real_stable_exe(),
        real_release_target(),
        real_current_exe(),
        real_download(),
        super::resume::spawn_update_resume,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn apply_update_impl<S>(
    manager: Arc<UpdateManager>,
    store: Arc<Store>,
    latest: Version,
    tx: ProgressTx,
    brew_upgrade: BrewUpgradeFn,
    version_check: VersionCheckFn,
    self_replace: SelfReplaceFn,
    release_fetch: ReleaseFetchFn,
    find_brew: FindBrewFn,
    stable_exe: StableExeFn,
    release_target_fn: ReleaseTargetFn,
    current_exe_fn: CurrentExeFn,
    download: DownloadFn,
    spawner: S,
) -> ApplyOutcome
where
    S: FnOnce(&super::RestartContext) -> Result<(), String>,
{
    let channel = {
        let g = manager.inner.lock().await;
        g.channel
    };

    let result = match channel {
        UpdateChannel::Homebrew => {
            apply_homebrew_impl(
                &latest,
                &tx,
                brew_upgrade,
                version_check,
                find_brew,
                stable_exe,
            )
            .await
        }
        UpdateChannel::Direct => {
            apply_direct_impl(
                &latest,
                &tx,
                self_replace,
                release_fetch,
                release_target_fn,
                current_exe_fn,
                download,
            )
            .await
        }
    };

    match result {
        Ok(()) => {
            if let Err(err) = store.spill_for_update().await {
                warn!(error = %err, "failed to spill log buffer before update restart");
            }
            let _ = tx.send(UpdateEvent {
                step: "Restarting Mizpah…".into(),
                progress: 0.95,
                error: None,
                restarting: Some(true),
            });
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Err(err) = spawner(manager.restart_context()) {
                warn!(error = %err, "failed to spawn update-resume helper");
                let _ = tx.send(UpdateEvent {
                    step: "Restart failed".into(),
                    progress: 0.95,
                    error: Some(err),
                    restarting: None,
                });
                manager.clear_busy().await;
                return ApplyOutcome::Failed;
            }
            ApplyOutcome::RestartRequested
        }
        Err(err) => {
            let _ = tx.send(UpdateEvent {
                step: "Update failed".into(),
                progress: 0.0,
                error: Some(err),
                restarting: None,
            });
            manager.clear_busy().await;
            ApplyOutcome::Failed
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

async fn apply_homebrew_impl(
    latest: &Version,
    tx: &ProgressTx,
    brew_upgrade: BrewUpgradeFn,
    version_check: VersionCheckFn,
    find_brew: FindBrewFn,
    stable_exe: StableExeFn,
) -> Result<(), String> {
    emit(tx, "Checking Homebrew…", 0.1);
    let brew = find_brew()
        .ok_or_else(|| "Homebrew install detected but `brew` was not found".to_string())?;

    emit(tx, "Running brew upgrade…", 0.35);
    let output = tokio::task::spawn_blocking({
        let brew = brew.clone();
        let upgrade_fn = Arc::clone(&brew_upgrade);
        move || upgrade_fn(&brew)
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
    let stable = stable_exe().map_err(|e| e.to_string())?;
    let ver_out = tokio::task::spawn_blocking({
        let stable = stable.clone();
        let check_fn = Arc::clone(&version_check);
        move || check_fn(&stable)
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

async fn apply_direct_impl(
    latest: &Version,
    tx: &ProgressTx,
    self_replace: SelfReplaceFn,
    release_fetch: ReleaseFetchFn,
    release_target_fn: ReleaseTargetFn,
    current_exe_fn: CurrentExeFn,
    download: DownloadFn,
) -> Result<(), String> {
    emit(tx, "Checking latest release…", 0.05);
    let target = release_target_fn().ok_or_else(|| {
        "No prebuilt binary for this platform. Install via Homebrew or build from source."
            .to_string()
    })?;

    let exe = current_exe_fn().map_err(|e| e.to_string())?;
    let install_dir = exe
        .parent()
        .ok_or_else(|| "could not determine install directory".to_string())?
        .to_path_buf();

    preflight_writable(&install_dir)?;

    let info = release_fetch().await?;
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
    download(url, archive_path.clone(), tx.clone()).await?;

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
        self_replace(new_running)?;
        if !same_inode && sibling_name != running_name.as_str() {
            atomic_replace_file(new_sibling, &sibling_dest)?;
        }
    } else {
        atomic_replace_file(new_sibling, &sibling_dest)?;
        self_replace(new_running)?;
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
    crate::util::ensure_rustls_crypto_provider();
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

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::path::PathBuf;
    use std::process::ExitStatus;
    use tar::Builder;

    use tokio::net::TcpListener;

    fn mock_brew_path() -> PathBuf {
        PathBuf::from("/mock/brew")
    }

    fn mock_stable_path() -> PathBuf {
        PathBuf::from("/mock/mizpah")
    }

    fn always_find_brew() -> FindBrewFn {
        Arc::new(|| Some(mock_brew_path()))
    }

    fn never_find_brew() -> FindBrewFn {
        Arc::new(|| None)
    }

    fn stable_exe_ok(path: PathBuf) -> StableExeFn {
        Arc::new(move || Ok(path.clone()))
    }

    fn stable_exe_err(msg: &'static str) -> StableExeFn {
        Arc::new(move || Err(io::Error::other(msg)))
    }

    fn ok_brew_upgrade() -> BrewUpgradeFn {
        Arc::new(|_: &Path| {
            Ok(Output {
                status: ExitStatus::default(),
                stdout: vec![],
                stderr: vec![],
            })
        })
    }

    fn version_check_output(stdout: &'static [u8]) -> VersionCheckFn {
        Arc::new(move |_: &Path| {
            Ok(Output {
                status: ExitStatus::default(),
                stdout: stdout.to_vec(),
                stderr: vec![],
            })
        })
    }

    #[cfg(unix)]
    fn failed_exit_status() -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(256)
    }

    #[cfg(not(unix))]
    fn failed_exit_status() -> ExitStatus {
        panic!("test only runs on unix")
    }

    fn write_mizpah_tarball(path: &Path) {
        let staging = tempfile::tempdir().expect("staging dir");
        let mizpah = staging.path().join("mizpah");
        let mzp = staging.path().join("mzp");
        fs::write(&mizpah, b"#!/bin/sh\necho mizpah\n").expect("write mizpah");
        fs::write(&mzp, b"#!/bin/sh\necho mzp\n").expect("write mzp");

        let file = File::create(path).expect("create archive");
        let enc = GzEncoder::new(file, Compression::default());
        let mut tar = Builder::new(enc);
        tar.append_path_with_name(&mizpah, "mizpah")
            .expect("append mizpah");
        tar.append_path_with_name(&mzp, "mzp").expect("append mzp");
        tar.finish().expect("finish tar");
    }

    fn write_bad_tarball(path: &Path) {
        let staging = tempfile::tempdir().expect("staging dir");
        let only = staging.path().join("readme.txt");
        fs::write(&only, b"not a binary").expect("write readme");

        let file = File::create(path).expect("create archive");
        let enc = GzEncoder::new(file, Compression::default());
        let mut tar = Builder::new(enc);
        tar.append_path_with_name(&only, "readme.txt")
            .expect("append readme");
        tar.finish().expect("finish tar");
    }

    #[cfg(not(miri))]
    async fn start_static_http(body: Vec<u8>, content_length: bool) -> String {
        use axum::body::Body;
        use axum::http::{header, Response, StatusCode};
        use axum::routing::get;
        use axum::Router;

        let app = Router::new().route(
            "/dl",
            get(move || {
                let body = body.clone();
                async move {
                    let mut builder = Response::builder().status(StatusCode::OK);
                    if content_length {
                        builder = builder.header(header::CONTENT_LENGTH, body.len());
                    }
                    builder
                        .header(header::CONTENT_TYPE, "application/octet-stream")
                        .body(Body::from(body))
                        .unwrap()
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        // Brief settle so accept is ready under load.
        tokio::task::yield_now().await;
        format!("http://{addr}/dl")
    }

    fn direct_deps(
        install_dir: PathBuf,
        running_name: &str,
    ) -> (ReleaseTargetFn, CurrentExeFn, DownloadFn) {
        let release_target_fn = Arc::new(|| Some("aarch64-apple-darwin".to_string()));
        let current_exe_fn = {
            let install_dir = install_dir.clone();
            let running = running_name.to_string();
            Arc::new(move || Ok(install_dir.join(&running)))
        };
        let download: DownloadFn = {
            Arc::new(move |_url: String, dest: PathBuf, _tx: ProgressTx| {
                let install_dir = install_dir.clone();
                Box::pin(async move {
                    let src = install_dir.join("fixture.tar.gz");
                    if src.is_file() {
                        fs::copy(&src, &dest).map_err(|e| e.to_string())?;
                    } else {
                        write_mizpah_tarball(&dest);
                    }
                    Ok(())
                }) as _
            })
        };
        (release_target_fn, current_exe_fn, download)
    }

    fn noop_direct_deps() -> (ReleaseTargetFn, CurrentExeFn, DownloadFn) {
        (
            Arc::new(|| Some("aarch64-apple-darwin".to_string())),
            Arc::new(|| Ok(PathBuf::from("/tmp/mizpah"))),
            Arc::new(|_, _, _| Box::pin(async { Ok(()) }) as _),
        )
    }

    #[tokio::test]
    async fn apply_homebrew_success() {
        let latest = Version::new(0, 9, 0);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let mock_brew_upgrade = Arc::new(|_: &Path| -> io::Result<Output> {
            Ok(Output {
                status: ExitStatus::default(),
                stdout: vec![],
                stderr: vec![],
            })
        });

        let mock_version_check = Arc::new(|_: &Path| -> io::Result<Output> {
            Ok(Output {
                status: ExitStatus::default(),
                stdout: b"mizpah 0.9.0\n".to_vec(),
                stderr: vec![],
            })
        });

        let mock_find_brew = Arc::new(|| Some(std::path::PathBuf::from("/usr/local/bin/brew")));
        let mock_stable_exe = Arc::new(|| Ok(std::path::PathBuf::from("/opt/homebrew/bin/mizpah")));

        let result = apply_homebrew_impl(
            &latest,
            &tx,
            mock_brew_upgrade,
            mock_version_check,
            mock_find_brew,
            mock_stable_exe,
        )
        .await;

        drop(tx);
        let mut events = vec![];
        while let Some(e) = rx.recv().await {
            events.push(e);
        }

        assert!(result.is_ok(), "expected success, got {result:?}");
        assert!(events.iter().any(|e| e.step.contains("Checking Homebrew")));
    }

    #[tokio::test]
    async fn apply_homebrew_upgrade_failed() {
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        #[cfg(unix)]
        fn make_failed_status() -> ExitStatus {
            use std::os::unix::process::ExitStatusExt;
            ExitStatus::from_raw(256)
        }

        #[cfg(not(unix))]
        fn make_failed_status() -> ExitStatus {
            panic!("test only runs on unix")
        }

        let mock_brew_upgrade = Arc::new(|_: &Path| -> io::Result<Output> {
            Ok(Output {
                status: make_failed_status(),
                stdout: vec![],
                stderr: b"brew error\n".to_vec(),
            })
        });

        let mock_version_check = Arc::new(|_: &Path| -> io::Result<Output> {
            Ok(Output {
                status: ExitStatus::default(),
                stdout: b"mizpah 0.9.0\n".to_vec(),
                stderr: vec![],
            })
        });

        let mock_find_brew = Arc::new(|| Some(std::path::PathBuf::from("/usr/local/bin/brew")));
        let mock_stable_exe = Arc::new(|| Ok(std::path::PathBuf::from("/opt/homebrew/bin/mizpah")));

        let result = apply_homebrew_impl(
            &latest,
            &tx,
            mock_brew_upgrade,
            mock_version_check,
            mock_find_brew,
            mock_stable_exe,
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("brew error"));
    }

    #[tokio::test]
    async fn apply_homebrew_version_mismatch() {
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let mock_brew_upgrade = Arc::new(|_: &Path| -> io::Result<Output> {
            Ok(Output {
                status: ExitStatus::default(),
                stdout: vec![],
                stderr: vec![],
            })
        });

        let mock_version_check = Arc::new(|_: &Path| -> io::Result<Output> {
            Ok(Output {
                status: ExitStatus::default(),
                stdout: b"mizpah 0.8.0\n".to_vec(),
                stderr: vec![],
            })
        });

        let mock_find_brew = Arc::new(|| Some(std::path::PathBuf::from("/usr/local/bin/brew")));
        let mock_stable_exe = Arc::new(|| Ok(std::path::PathBuf::from("/opt/homebrew/bin/mizpah")));

        let result = apply_homebrew_impl(
            &latest,
            &tx,
            mock_brew_upgrade,
            mock_version_check,
            mock_find_brew,
            mock_stable_exe,
        )
        .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("0.8.0"));
        assert!(err.contains("0.9.0"));
    }

    #[tokio::test]
    async fn apply_direct_success() {
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let mock_self_replace = Arc::new(|_: &Path| -> Result<(), String> { Ok(()) });

        let mock_release_fetch = Arc::new(|| {
            Box::pin(async {
                Ok(ReleaseInfo {
                    version: Version::new(0, 9, 0),
                    download_url: None,
                    body: None,
                })
            })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<ReleaseInfo, String>> + Send>,
                >
        });

        let mock_release_target = Arc::new(|| Some("aarch64-apple-darwin".into()));
        let mock_current_exe = Arc::new(|| Ok(std::env::current_exe().unwrap()));
        let mock_download = Arc::new(|_: String, _: std::path::PathBuf, _: ProgressTx| {
            Box::pin(async { Ok(()) })
                as std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
        });

        let result = apply_direct_impl(
            &latest,
            &tx,
            mock_self_replace,
            mock_release_fetch,
            mock_release_target,
            mock_current_exe,
            mock_download,
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("No prebuilt binary") || err.contains("no asset"));
    }

    #[tokio::test]
    async fn apply_update_impl_restart_outcome() {
        let store = Arc::new(crate::store::Store::new(1024));
        let manager = crate::update::UpdateManager::new(crate::update::RestartContext {
            host: "127.0.0.1".into(),
            port: 3149,
            project_dir: std::env::temp_dir(),
            max_bytes: 1024,
            ttl_hours: 1,
        });
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        {
            let mut g = manager.inner.lock().await;
            g.channel = UpdateChannel::Homebrew;
            g.latest_version = Some(Version::new(0, 9, 0));
            g.installed_version = Version::new(0, 8, 0);
        }

        let mock_brew_upgrade = Arc::new(|_: &Path| -> io::Result<Output> {
            Ok(Output {
                status: ExitStatus::default(),
                stdout: vec![],
                stderr: vec![],
            })
        });

        let mock_version_check = Arc::new(|_: &Path| -> io::Result<Output> {
            Ok(Output {
                status: ExitStatus::default(),
                stdout: b"mizpah 0.9.0\n".to_vec(),
                stderr: vec![],
            })
        });

        let mock_self_replace = Arc::new(|_: &Path| -> Result<(), String> { Ok(()) });

        let mock_release_fetch = Arc::new(|| {
            Box::pin(async {
                Ok(ReleaseInfo {
                    version: Version::new(0, 9, 0),
                    download_url: Some("http://example.com/archive.tar.gz".into()),
                    body: None,
                })
            })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<ReleaseInfo, String>> + Send>,
                >
        });

        let spawner = |_: &crate::update::RestartContext| Ok(());
        let (release_target_fn, current_exe_fn, download) = noop_direct_deps();

        let outcome = apply_update_impl(
            manager,
            store,
            latest,
            tx,
            mock_brew_upgrade,
            mock_version_check,
            mock_self_replace,
            mock_release_fetch,
            always_find_brew(),
            stable_exe_ok(mock_stable_path()),
            release_target_fn,
            current_exe_fn,
            download,
            spawner,
        )
        .await;

        assert_eq!(outcome, ApplyOutcome::RestartRequested);
    }

    #[tokio::test]
    async fn apply_update_impl_failed_outcome() {
        let store = Arc::new(crate::store::Store::new(1024));
        let manager = crate::update::UpdateManager::new(crate::update::RestartContext {
            host: "127.0.0.1".into(),
            port: 3149,
            project_dir: std::env::temp_dir(),
            max_bytes: 1024,
            ttl_hours: 1,
        });
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        {
            let mut g = manager.inner.lock().await;
            g.channel = UpdateChannel::Homebrew;
            g.latest_version = Some(Version::new(0, 9, 0));
            g.installed_version = Version::new(0, 8, 0);
        }

        #[cfg(unix)]
        fn make_failed_status() -> ExitStatus {
            use std::os::unix::process::ExitStatusExt;
            ExitStatus::from_raw(256)
        }

        #[cfg(not(unix))]
        fn make_failed_status() -> ExitStatus {
            panic!("test only runs on unix")
        }

        let mock_brew_upgrade = Arc::new(|_: &Path| -> io::Result<Output> {
            Ok(Output {
                status: make_failed_status(),
                stdout: vec![],
                stderr: b"upgrade failed\n".to_vec(),
            })
        });

        let mock_version_check = Arc::new(|_: &Path| -> io::Result<Output> {
            Ok(Output {
                status: ExitStatus::default(),
                stdout: b"mizpah 0.9.0\n".to_vec(),
                stderr: vec![],
            })
        });

        let mock_self_replace = Arc::new(|_: &Path| -> Result<(), String> { Ok(()) });

        let mock_release_fetch = Arc::new(|| {
            Box::pin(async {
                Ok(ReleaseInfo {
                    version: Version::new(0, 9, 0),
                    download_url: Some("http://example.com/archive.tar.gz".into()),
                    body: None,
                })
            })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<ReleaseInfo, String>> + Send>,
                >
        });

        let spawner = |_: &crate::update::RestartContext| Ok(());
        let (release_target_fn, current_exe_fn, download) = noop_direct_deps();

        let outcome = apply_update_impl(
            manager,
            store,
            latest,
            tx,
            mock_brew_upgrade,
            mock_version_check,
            mock_self_replace,
            mock_release_fetch,
            always_find_brew(),
            stable_exe_ok(mock_stable_path()),
            release_target_fn,
            current_exe_fn,
            download,
            spawner,
        )
        .await;

        assert_eq!(outcome, ApplyOutcome::Failed);
    }

    #[test]
    fn truncate_err_short() {
        assert_eq!(truncate_err("short"), "short");
    }

    #[test]
    fn truncate_err_long() {
        let long = "a".repeat(500);
        let truncated = truncate_err(&long);
        assert!(truncated.len() < long.len());
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn preflight_writable_success() {
        let dir = std::env::temp_dir();
        assert!(preflight_writable(&dir).is_ok());
    }

    #[test]
    fn preflight_writable_nonexistent() {
        let dir = std::env::temp_dir().join("nonexistent-dir-12345");
        assert!(preflight_writable(&dir).is_err());
    }

    #[tokio::test]
    async fn apply_homebrew_brew_not_found() {
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let result = apply_homebrew_impl(
            &latest,
            &tx,
            ok_brew_upgrade(),
            version_check_output(b"mizpah 0.9.0\n"),
            never_find_brew(),
            stable_exe_ok(mock_stable_path()),
        )
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("brew` was not found"));
    }

    #[tokio::test]
    async fn apply_homebrew_upgrade_failed_empty_stderr() {
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let mock_brew_upgrade = Arc::new(|_: &Path| {
            Ok(Output {
                status: failed_exit_status(),
                stdout: vec![],
                stderr: vec![],
            })
        });

        let result = apply_homebrew_impl(
            &latest,
            &tx,
            mock_brew_upgrade,
            version_check_output(b"mizpah 0.9.0\n"),
            always_find_brew(),
            stable_exe_ok(mock_stable_path()),
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("brew upgrade failed"));
    }

    #[tokio::test]
    async fn apply_homebrew_upgrade_io_error() {
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let mock_brew_upgrade =
            Arc::new(|_: &Path| Err(io::Error::new(io::ErrorKind::NotFound, "missing brew")));

        let result = apply_homebrew_impl(
            &latest,
            &tx,
            mock_brew_upgrade,
            version_check_output(b"mizpah 0.9.0\n"),
            always_find_brew(),
            stable_exe_ok(mock_stable_path()),
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("failed to run brew"));
    }

    #[tokio::test]
    async fn apply_homebrew_stable_exe_error() {
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let result = apply_homebrew_impl(
            &latest,
            &tx,
            ok_brew_upgrade(),
            version_check_output(b"mizpah 0.9.0\n"),
            always_find_brew(),
            stable_exe_err("no stable exe"),
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no stable exe"));
    }

    #[tokio::test]
    async fn apply_homebrew_version_check_io_error() {
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let mock_version_check =
            Arc::new(|_: &Path| Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")));

        let result = apply_homebrew_impl(
            &latest,
            &tx,
            ok_brew_upgrade(),
            mock_version_check,
            always_find_brew(),
            stable_exe_ok(mock_stable_path()),
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("failed to run --version"));
    }

    #[tokio::test]
    async fn apply_homebrew_version_parse_error() {
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let result = apply_homebrew_impl(
            &latest,
            &tx,
            ok_brew_upgrade(),
            version_check_output(b"not-a-version\n"),
            always_find_brew(),
            stable_exe_ok(mock_stable_path()),
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("could not parse version"));
    }

    #[tokio::test]
    async fn apply_direct_no_release_target() {
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let result = apply_direct_impl(
            &latest,
            &tx,
            Arc::new(|_: &Path| Ok(())),
            Arc::new(|| {
                Box::pin(async {
                    Ok(ReleaseInfo {
                        version: Version::new(0, 9, 0),
                        download_url: None,
                        body: None,
                    })
                }) as _
            }),
            Arc::new(|| None),
            Arc::new(|| Ok(PathBuf::from("/tmp/mizpah"))),
            Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        )
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No prebuilt binary"));
    }

    #[tokio::test]
    async fn apply_direct_release_fetch_error() {
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let install = tempfile::tempdir().expect("install dir");
        fs::write(install.path().join("mizpah"), b"old").expect("seed exe");
        let (release_target_fn, current_exe_fn, download) =
            direct_deps(install.path().to_path_buf(), "mizpah");

        let result = apply_direct_impl(
            &latest,
            &tx,
            Arc::new(|_: &Path| Ok(())),
            Arc::new(|| Box::pin(async { Err("github down".into()) }) as _),
            release_target_fn,
            current_exe_fn,
            download,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "github down");
    }

    #[tokio::test]
    async fn apply_direct_success_mizpah() {
        let latest = Version::new(0, 9, 0);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let install = tempfile::tempdir().expect("install dir");
        fs::write(install.path().join("mizpah"), b"old").expect("seed exe");
        let (release_target_fn, current_exe_fn, download) =
            direct_deps(install.path().to_path_buf(), "mizpah");

        let result = apply_direct_impl(
            &latest,
            &tx,
            Arc::new(|_: &Path| Ok(())),
            Arc::new(|| {
                Box::pin(async {
                    Ok(ReleaseInfo {
                        version: Version::new(0, 8, 0),
                        download_url: Some("http://example.com/archive.tar.gz".into()),
                        body: None,
                    })
                }) as _
            }),
            release_target_fn,
            current_exe_fn,
            download,
        )
        .await;

        drop(tx);
        let mut saw_install = false;
        while let Some(event) = rx.recv().await {
            if event.step.contains("Installing binaries") {
                saw_install = true;
            }
        }
        assert!(result.is_ok(), "expected success, got {result:?}");
        assert!(saw_install);
    }

    #[tokio::test]
    async fn apply_direct_success_mzp() {
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let install = tempfile::tempdir().expect("install dir");
        fs::write(install.path().join("mzp"), b"old").expect("seed exe");
        let (release_target_fn, current_exe_fn, download) =
            direct_deps(install.path().to_path_buf(), "mzp");

        let result = apply_direct_impl(
            &latest,
            &tx,
            Arc::new(|p: &Path| {
                assert!(p.ends_with("mzp"));
                Ok(())
            }),
            Arc::new(|| {
                Box::pin(async {
                    Ok(ReleaseInfo {
                        version: Version::new(0, 9, 0),
                        download_url: Some("http://example.com/archive.tar.gz".into()),
                        body: None,
                    })
                }) as _
            }),
            release_target_fn,
            current_exe_fn,
            download,
        )
        .await;

        assert!(result.is_ok(), "expected success, got {result:?}");
    }

    #[tokio::test]
    async fn apply_direct_same_inode_replaces_running_only() {
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let install = tempfile::tempdir().expect("install dir");
        let exe = install.path().join("mizpah");
        fs::write(&exe, b"old").expect("seed exe");
        fs::hard_link(&exe, install.path().join("mzp")).expect("hard link sibling");
        let (release_target_fn, current_exe_fn, download) =
            direct_deps(install.path().to_path_buf(), "mizpah");

        let result = apply_direct_impl(
            &latest,
            &tx,
            Arc::new(|_: &Path| Ok(())),
            Arc::new(|| {
                Box::pin(async {
                    Ok(ReleaseInfo {
                        version: Version::new(0, 9, 0),
                        download_url: Some("http://example.com/archive.tar.gz".into()),
                        body: None,
                    })
                }) as _
            }),
            release_target_fn,
            current_exe_fn,
            download,
        )
        .await;

        assert!(result.is_ok(), "expected success, got {result:?}");
    }

    #[tokio::test]
    async fn apply_direct_separate_binaries_replace_sibling_first() {
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let install = tempfile::tempdir().expect("install dir");
        fs::write(install.path().join("mizpah"), b"old").expect("seed running");
        fs::write(install.path().join("mzp"), b"old sibling").expect("seed sibling");
        let (release_target_fn, current_exe_fn, download) =
            direct_deps(install.path().to_path_buf(), "mizpah");

        let result = apply_direct_impl(
            &latest,
            &tx,
            Arc::new(|_: &Path| Ok(())),
            Arc::new(|| {
                Box::pin(async {
                    Ok(ReleaseInfo {
                        version: Version::new(0, 9, 0),
                        download_url: Some("http://example.com/archive.tar.gz".into()),
                        body: None,
                    })
                }) as _
            }),
            release_target_fn,
            current_exe_fn,
            download,
        )
        .await;

        assert!(result.is_ok(), "expected success, got {result:?}");
        assert!(install.path().join("mzp").is_file());
    }

    #[tokio::test]
    async fn apply_direct_missing_binaries_in_archive() {
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let install = tempfile::tempdir().expect("install dir");
        fs::write(install.path().join("mizpah"), b"old").expect("seed exe");
        write_bad_tarball(&install.path().join("fixture.tar.gz"));
        let (release_target_fn, current_exe_fn, download) =
            direct_deps(install.path().to_path_buf(), "mizpah");

        let result = apply_direct_impl(
            &latest,
            &tx,
            Arc::new(|_: &Path| Ok(())),
            Arc::new(|| {
                Box::pin(async {
                    Ok(ReleaseInfo {
                        version: Version::new(0, 9, 0),
                        download_url: Some("http://example.com/archive.tar.gz".into()),
                        body: None,
                    })
                }) as _
            }),
            release_target_fn,
            current_exe_fn,
            download,
        )
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("archive missing"));
    }

    #[tokio::test]
    async fn apply_direct_self_replace_error() {
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let install = tempfile::tempdir().expect("install dir");
        fs::write(install.path().join("mizpah"), b"old").expect("seed exe");
        let (release_target_fn, current_exe_fn, download) =
            direct_deps(install.path().to_path_buf(), "mizpah");

        let result = apply_direct_impl(
            &latest,
            &tx,
            Arc::new(|_: &Path| Err("replace denied".into())),
            Arc::new(|| {
                Box::pin(async {
                    Ok(ReleaseInfo {
                        version: Version::new(0, 9, 0),
                        download_url: Some("http://example.com/archive.tar.gz".into()),
                        body: None,
                    })
                }) as _
            }),
            release_target_fn,
            current_exe_fn,
            download,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "replace denied");
    }

    #[tokio::test]
    async fn apply_update_impl_spawner_failure() {
        let store = Arc::new(crate::store::Store::new(1024));
        let manager = crate::update::UpdateManager::new(crate::update::RestartContext {
            host: "127.0.0.1".into(),
            port: 3149,
            project_dir: std::env::temp_dir(),
            max_bytes: 1024,
            ttl_hours: 1,
        });
        let latest = Version::new(0, 9, 0);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        {
            let mut g = manager.inner.lock().await;
            g.channel = UpdateChannel::Homebrew;
        }

        let (release_target_fn, current_exe_fn, download) = noop_direct_deps();

        let outcome = apply_update_impl(
            Arc::clone(&manager),
            store,
            latest,
            tx,
            ok_brew_upgrade(),
            version_check_output(b"mizpah 0.9.0\n"),
            Arc::new(|_: &Path| Ok(())),
            Arc::new(|| {
                Box::pin(async {
                    Ok(ReleaseInfo {
                        version: Version::new(0, 9, 0),
                        download_url: Some("http://example.com/archive.tar.gz".into()),
                        body: None,
                    })
                }) as _
            }),
            always_find_brew(),
            stable_exe_ok(mock_stable_path()),
            release_target_fn,
            current_exe_fn,
            download,
            |_: &crate::update::RestartContext| Err("spawn failed".into()),
        )
        .await;

        assert_eq!(outcome, ApplyOutcome::Failed);
        let mut restart_failed = None;
        while let Some(event) = rx.recv().await {
            if event.step == "Restart failed" {
                restart_failed = Some(event);
            }
        }
        let event = restart_failed.expect("restart failed event");
        assert_eq!(event.error.as_deref(), Some("spawn failed"));
        assert!(!manager.inner.lock().await.busy);
    }

    #[tokio::test]
    async fn apply_update_impl_direct_channel() {
        let store = Arc::new(crate::store::Store::new(1024));
        let manager = crate::update::UpdateManager::new(crate::update::RestartContext {
            host: "127.0.0.1".into(),
            port: 3149,
            project_dir: std::env::temp_dir(),
            max_bytes: 1024,
            ttl_hours: 1,
        });
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        {
            let mut g = manager.inner.lock().await;
            g.channel = UpdateChannel::Direct;
        }

        let (release_target_fn, current_exe_fn, download) = noop_direct_deps();

        let outcome = apply_update_impl(
            manager,
            store,
            latest,
            tx,
            ok_brew_upgrade(),
            version_check_output(b"mizpah 0.9.0\n"),
            Arc::new(|_: &Path| Ok(())),
            Arc::new(|| {
                Box::pin(async {
                    Ok(ReleaseInfo {
                        version: Version::new(0, 9, 0),
                        download_url: None,
                        body: None,
                    })
                }) as _
            }),
            always_find_brew(),
            stable_exe_ok(mock_stable_path()),
            release_target_fn,
            current_exe_fn,
            download,
            |_: &crate::update::RestartContext| Ok(()),
        )
        .await;

        assert_eq!(outcome, ApplyOutcome::Failed);
    }

    #[test]
    fn real_injectors_construct() {
        let _ = real_brew_upgrade();
        let _ = real_version_check();
        let _ = real_self_replace();
        let _ = real_release_fetch();
        let _ = real_find_brew();
        let _ = real_stable_exe();
        let _ = real_release_target();
        let _ = real_current_exe();
        let _ = real_download();
    }

    #[test]
    fn extract_tarball_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = dir.path().join("bundle.tar.gz");
        write_mizpah_tarball(&archive);
        let extract_dir = dir.path().join("out");
        fs::create_dir_all(&extract_dir).expect("mkdir");
        extract_tarball(&archive, &extract_dir).expect("extract");
        assert!(extract_dir.join("mizpah").is_file());
        assert!(extract_dir.join("mzp").is_file());
    }

    #[test]
    fn set_executable_sets_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("tool");
        fs::write(&bin, b"#!/bin/sh\n").expect("write");
        set_executable(&bin).expect("chmod");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&bin).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
    }

    #[test]
    fn atomic_replace_file_success() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src-bin");
        let dest = dir.path().join("dest-bin");
        fs::write(&src, b"new bytes").expect("write src");
        fs::write(&dest, b"old bytes").expect("write dest");
        atomic_replace_file(&src, &dest).expect("replace");
        assert_eq!(fs::read(&dest).expect("read dest"), b"new bytes");
    }

    #[test]
    fn atomic_replace_file_invalid_dest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src-bin");
        fs::write(&src, b"x").expect("write src");
        let dest = Path::new("/no/such/parent/dest-bin");
        assert!(atomic_replace_file(&src, dest).is_err());
    }

    #[test]
    fn same_file_detects_hard_link_and_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        let missing = dir.path().join("missing");
        fs::write(&a, b"x").expect("write a");
        fs::hard_link(&a, &b).expect("hard link");
        assert!(same_file(&a, &b).expect("same"));
        assert!(!same_file(&a, &missing).expect("missing"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn clear_quarantine_is_best_effort() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("bin");
        fs::write(&bin, b"x").expect("write");
        clear_quarantine(&bin);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn download_with_progress_writes_file_with_content_length() {
        let body = b"hello-update-bytes".to_vec();
        let url = start_static_http(body.clone(), true).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("download.bin");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        download_with_progress(&url, &dest, &tx)
            .await
            .expect("download");
        assert_eq!(fs::read(&dest).expect("read"), body);

        drop(tx);
        let mut saw_progress = false;
        while let Some(event) = rx.recv().await {
            if event.step.contains("Downloading") && event.progress > 0.15 {
                saw_progress = true;
            }
        }
        assert!(saw_progress);
    }

    #[tokio::test]
    async fn apply_direct_install_dir_not_writable() {
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let install = tempfile::tempdir().expect("install dir");
        fs::write(install.path().join("mizpah"), b"old").expect("seed exe");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(install.path()).expect("meta").permissions();
            perms.set_mode(0o555);
            fs::set_permissions(install.path(), perms).expect("chmod");
        }
        #[cfg(not(unix))]
        return;

        let (release_target_fn, current_exe_fn, download) =
            direct_deps(install.path().to_path_buf(), "mizpah");

        let result = apply_direct_impl(
            &latest,
            &tx,
            Arc::new(|_: &Path| Ok(())),
            Arc::new(|| {
                Box::pin(async {
                    Ok(ReleaseInfo {
                        version: Version::new(0, 9, 0),
                        download_url: Some("http://example.com/archive.tar.gz".into()),
                        body: None,
                    })
                }) as _
            }),
            release_target_fn,
            current_exe_fn,
            download,
        )
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not writable"));
    }

    #[tokio::test]
    async fn apply_direct_current_exe_has_no_parent() {
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let result = apply_direct_impl(
            &latest,
            &tx,
            Arc::new(|_: &Path| Ok(())),
            Arc::new(|| {
                Box::pin(async {
                    Ok(ReleaseInfo {
                        version: Version::new(0, 9, 0),
                        download_url: Some("http://example.com/x.tar.gz".into()),
                        body: None,
                    })
                }) as _
            }),
            Arc::new(|| Some("aarch64-apple-darwin".to_string())),
            Arc::new(|| Ok(PathBuf::from(""))),
            Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("could not determine") || err.contains("not writable"),
            "unexpected err: {err}"
        );
    }

    #[cfg(not(miri))]
    #[test]
    fn real_download_delegates_to_download_with_progress() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let body = b"via-real-download".to_vec();
            let url = start_static_http(body.clone(), true).await;
            let dir = tempfile::tempdir().expect("tempdir");
            let dest = dir.path().join("download.bin");
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let download = real_download();
            download(url, dest.clone(), tx).await.expect("download");
            assert_eq!(fs::read(dest).expect("read"), body);
        });
    }

    #[tokio::test]
    async fn apply_direct_missing_download_url() {
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let install = tempfile::tempdir().expect("install dir");
        fs::write(install.path().join("mizpah"), b"old").expect("seed exe");
        let (release_target_fn, current_exe_fn, download) =
            direct_deps(install.path().to_path_buf(), "mizpah");

        let result = apply_direct_impl(
            &latest,
            &tx,
            Arc::new(|_: &Path| Ok(())),
            Arc::new(|| {
                Box::pin(async {
                    Ok(ReleaseInfo {
                        version: Version::new(0, 9, 0),
                        download_url: None,
                        body: None,
                    })
                }) as _
            }),
            release_target_fn,
            current_exe_fn,
            download,
        )
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no asset"));
    }

    #[tokio::test]
    async fn apply_direct_download_error() {
        let latest = Version::new(0, 9, 0);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let install = tempfile::tempdir().expect("install dir");
        fs::write(install.path().join("mizpah"), b"old").expect("seed exe");
        let release_target_fn = Arc::new(|| Some("aarch64-apple-darwin".to_string()));
        let current_exe_fn = {
            let install_dir = install.path().to_path_buf();
            Arc::new(move || Ok(install_dir.join("mizpah")))
        };
        let download = Arc::new(|_: String, _: PathBuf, _: ProgressTx| {
            Box::pin(async { Err("download failed".into()) }) as _
        });

        let result = apply_direct_impl(
            &latest,
            &tx,
            Arc::new(|_: &Path| Ok(())),
            Arc::new(|| {
                Box::pin(async {
                    Ok(ReleaseInfo {
                        version: Version::new(0, 9, 0),
                        download_url: Some("http://example.com/x.tar.gz".into()),
                        body: None,
                    })
                }) as _
            }),
            release_target_fn,
            current_exe_fn,
            download,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "download failed");
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn download_with_progress_without_content_length() {
        let body = b"chunked-body".to_vec();
        let url = start_static_http(body.clone(), false).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("download.bin");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        download_with_progress(&url, &dest, &tx)
            .await
            .expect("download");
        assert_eq!(fs::read(&dest).expect("read"), body);

        drop(tx);
        let mut saw_downloading = false;
        while let Some(event) = rx.recv().await {
            if event.step.contains("Downloading") {
                saw_downloading = true;
                assert!(event.progress >= 0.15 && event.progress <= 0.7);
            }
        }
        assert!(saw_downloading);
    }
}
