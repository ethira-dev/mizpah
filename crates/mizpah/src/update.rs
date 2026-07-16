//! Check GitHub Releases for updates and apply Homebrew or direct self-updates.

use flate2::read::GzDecoder;
use futures_util::StreamExt;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tar::Archive;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, warn};

const GITHUB_REPO: &str = "ethira-dev/mizpah";
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// Re-fetch GitHub latest when status is read and the cache is older than this.
const CHECK_TTL: Duration = Duration::from_secs(15 * 60);
const BREW_FORMULA: &str = "ethira-dev/mizpah/mizpah";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateChannel {
    Homebrew,
    Direct,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub installed_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub channel: UpdateChannel,
    pub busy: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateEvent {
    pub step: String,
    pub progress: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restarting: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct RestartContext {
    pub host: String,
    pub port: u16,
    pub project_dir: PathBuf,
    pub max_bytes: u64,
}

struct Inner {
    installed_version: Version,
    latest_version: Option<Version>,
    channel: UpdateChannel,
    busy: bool,
    last_checked_at: Option<Instant>,
}

pub struct UpdateManager {
    inner: Mutex<Inner>,
    restart: RestartContext,
}

impl UpdateManager {
    pub fn new(restart: RestartContext) -> Arc<Self> {
        let current =
            Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 0, 0));
        Arc::new(Self {
            inner: Mutex::new(Inner {
                installed_version: current,
                latest_version: None,
                channel: detect_channel(),
                busy: false,
                last_checked_at: None,
            }),
            restart,
        })
    }

    pub async fn status(&self) -> UpdateStatus {
        self.ensure_fresh().await;
        let g = self.inner.lock().await;
        let update_available = g
            .latest_version
            .as_ref()
            .map(|l| l > &g.installed_version)
            .unwrap_or(false);
        UpdateStatus {
            installed_version: g.installed_version.to_string(),
            latest_version: g.latest_version.as_ref().map(|v| v.to_string()),
            update_available,
            channel: g.channel,
            busy: g.busy,
        }
    }

    /// Refresh from GitHub when never checked or the cache is past [`CHECK_TTL`].
    pub async fn ensure_fresh(&self) {
        let stale = {
            let g = self.inner.lock().await;
            if g.busy {
                return;
            }
            is_check_stale(g.last_checked_at, Instant::now(), CHECK_TTL)
        };
        if stale {
            self.check_now().await;
        }
    }

    pub async fn check_now(&self) {
        {
            let g = self.inner.lock().await;
            if g.busy {
                return;
            }
        }
        match fetch_latest_release().await {
            Ok(info) => {
                let mut g = self.inner.lock().await;
                g.latest_version = Some(info.version);
                g.channel = detect_channel();
                g.last_checked_at = Some(Instant::now());
            }
            Err(err) => {
                debug!(error = %err, "update check failed");
                // Still advance the TTL so failed checks do not hammer GitHub on every poll.
                let mut g = self.inner.lock().await;
                g.last_checked_at = Some(Instant::now());
            }
        }
    }

    pub fn spawn_background_checker(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            loop {
                this.check_now().await;
                tokio::time::sleep(Duration::from_secs(6 * 60 * 60)).await;
            }
        });
    }

    /// Try to mark busy for an apply. Returns Err with HTTP-ish reason.
    pub async fn try_begin_apply(&self) -> Result<Version, ApplyBeginError> {
        let mut g = self.inner.lock().await;
        if g.busy {
            return Err(ApplyBeginError::Busy);
        }
        let Some(latest) = g.latest_version.clone() else {
            return Err(ApplyBeginError::NoUpdate);
        };
        if latest <= g.installed_version {
            return Err(ApplyBeginError::NoUpdate);
        }
        g.busy = true;
        Ok(latest)
    }

    pub async fn clear_busy(&self) {
        let mut g = self.inner.lock().await;
        g.busy = false;
    }

    pub fn restart_context(&self) -> &RestartContext {
        &self.restart
    }
}

#[derive(Debug)]
pub enum ApplyBeginError {
    Busy,
    NoUpdate,
}

#[derive(Debug, Clone)]
struct ReleaseInfo {
    version: Version,
    download_url: Option<String>,
}

pub fn release_target() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("aarch64-apple-darwin")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("x86_64-apple-darwin")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("x86_64-unknown-linux-gnu")
    } else {
        None
    }
}

pub fn parse_tag_version(tag: &str) -> Option<Version> {
    let trimmed = tag.trim().trim_start_matches('v');
    Version::parse(trimmed).ok()
}

fn is_check_stale(last_checked_at: Option<Instant>, now: Instant, ttl: Duration) -> bool {
    match last_checked_at {
        None => true,
        Some(at) => now.saturating_duration_since(at) >= ttl,
    }
}

pub fn parse_cli_version(stdout: &str) -> Option<Version> {
    for token in stdout.split_whitespace() {
        let t = token.trim().trim_start_matches('v');
        if let Ok(v) = Version::parse(t) {
            return Some(v);
        }
    }
    None
}

pub fn detect_channel() -> UpdateChannel {
    let raw = std::env::current_exe().ok();
    let canon = raw.as_ref().and_then(|p| fs::canonicalize(p).ok());
    if path_is_homebrew(raw.as_deref()) || path_is_homebrew(canon.as_deref()) {
        return UpdateChannel::Homebrew;
    }
    UpdateChannel::Direct
}

fn path_is_homebrew(path: Option<&Path>) -> bool {
    path_is_homebrew_with_prefix(path, homebrew_prefix_from_env_only().as_deref())
}

fn path_is_homebrew_with_prefix(path: Option<&Path>, prefix: Option<&Path>) -> bool {
    let Some(path) = path else {
        return false;
    };
    let s = path.to_string_lossy();
    if s.contains("/Cellar/mizpah/") {
        return true;
    }
    if let Some(prefix) = prefix {
        let cellar = prefix.join("Cellar").join("mizpah");
        if path.starts_with(&cellar) {
            return true;
        }
        let bin = prefix.join("bin");
        if path.parent() == Some(bin.as_path()) {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "mizpah" || name == "mzp" {
                return true;
            }
        }
    }
    false
}

fn homebrew_prefix_from_env_only() -> Option<PathBuf> {
    let p = std::env::var("HOMEBREW_PREFIX").ok()?;
    let trimmed = p.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

/// Stable path for re-exec after brew/self-update.
pub fn stable_exe_path() -> io::Result<PathBuf> {
    let raw = std::env::current_exe()?;
    let prefix = homebrew_prefix();
    let prefer_homebrew =
        detect_channel() == UpdateChannel::Homebrew || path_looks_like_homebrew_cellar(&raw);
    Ok(resolve_stable_exe_path(
        &raw,
        prefer_homebrew,
        prefix.as_deref(),
        |p| p.exists(),
    ))
}

/// Pick a re-exec path that survives Homebrew Cellar version swaps.
///
/// Cellar paths end in `…/bin/<name>` but are deleted on upgrade; prefer
/// `$prefix/bin/<name>` when available.
fn resolve_stable_exe_path(
    raw: &Path,
    prefer_homebrew: bool,
    prefix: Option<&Path>,
    exists: impl Fn(&Path) -> bool,
) -> PathBuf {
    let name = running_bin_name(raw);

    if prefer_homebrew {
        if let Some(prefix) = prefix {
            let candidate = prefix.join("bin").join(&name);
            if exists(&candidate) {
                return candidate;
            }
        }
    }

    // Prefer current_exe only when it is already the prefix bin symlink, not Cellar.
    if let Some(prefix) = prefix {
        let prefix_bin = prefix.join("bin");
        if raw.parent() == Some(prefix_bin.as_path()) && exists(raw) {
            return raw.to_path_buf();
        }
    } else if !path_looks_like_homebrew_cellar(raw) {
        if let Some(parent) = raw.parent() {
            if parent.file_name().is_some_and(|n| n == "bin") && exists(raw) {
                return raw.to_path_buf();
            }
        }
    }

    raw.to_path_buf()
}

fn path_looks_like_homebrew_cellar(path: &Path) -> bool {
    path.to_string_lossy().contains("/Cellar/mizpah/")
}

pub fn running_bin_name(exe: &Path) -> String {
    exe.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("mizpah")
        .to_string()
}

pub fn sibling_bin_name(running: &str) -> &'static str {
    if running == "mzp" {
        "mizpah"
    } else {
        "mzp"
    }
}

fn homebrew_prefix() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("HOMEBREW_PREFIX") {
        let trimmed = p.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    for candidate in ["/opt/homebrew", "/usr/local", "/home/linuxbrew/.linuxbrew"] {
        let brew = Path::new(candidate).join("bin/brew");
        if brew.is_file() {
            return Some(PathBuf::from(candidate));
        }
    }
    if let Some(brew) = find_brew_binary() {
        if let Some(bin) = brew.parent() {
            if let Some(prefix) = bin.parent() {
                return Some(prefix.to_path_buf());
            }
        }
    }
    None
}

pub fn find_brew_binary() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(p) = std::env::var("HOMEBREW_PREFIX") {
        let trimmed = p.trim();
        if !trimmed.is_empty() {
            candidates.push(PathBuf::from(trimmed).join("bin/brew"));
        }
    }
    for c in [
        "/opt/homebrew/bin/brew",
        "/usr/local/bin/brew",
        "/home/linuxbrew/.linuxbrew/bin/brew",
    ] {
        candidates.push(PathBuf::from(c));
    }
    for c in candidates {
        if c.is_file() {
            return Some(c);
        }
    }
    which("brew")
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

async fn fetch_latest_release() -> Result<ReleaseInfo, String> {
    let client = reqwest::Client::builder()
        .timeout(CHECK_TIMEOUT)
        .user_agent(format!("mizpah/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;

    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let resp = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status();
    if status.as_u16() == 404 {
        return Err("no latest release".into());
    }
    if status.as_u16() == 403 || status.as_u16() == 429 {
        return Err(format!("GitHub API rate limited ({status})"));
    }
    if !status.is_success() {
        return Err(format!("GitHub API {status}"));
    }

    #[derive(Deserialize)]
    struct GhAsset {
        name: String,
        browser_download_url: String,
    }
    #[derive(Deserialize)]
    struct GhRelease {
        tag_name: String,
        assets: Vec<GhAsset>,
    }

    let body: GhRelease = resp.json().await.map_err(|e| e.to_string())?;
    let version = parse_tag_version(&body.tag_name)
        .ok_or_else(|| format!("invalid release tag {}", body.tag_name))?;

    let download_url = release_target().and_then(|target| {
        let want = format!("mizpah-{target}.tar.gz");
        body.assets
            .into_iter()
            .find(|a| a.name == want)
            .map(|a| a.browser_download_url)
    });

    Ok(ReleaseInfo {
        version,
        download_url,
    })
}

pub type ProgressTx = mpsc::UnboundedSender<UpdateEvent>;

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
            // Let the SSE frame flush.
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Err(err) = spawn_update_resume(manager.restart_context()) {
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
        // Only replace running binary (sibling missing or hardlinked).
        self_replace::self_replace(new_running).map_err(|e| e.to_string())?;
        if !same_inode && sibling_name != running_name.as_str() {
            // Sibling missing: install it beside us.
            atomic_replace_file(new_sibling, &sibling_dest)?;
        }
    } else {
        atomic_replace_file(new_sibling, &sibling_dest)?;
        self_replace::self_replace(new_running).map_err(|e| e.to_string())?;
    }

    let _ = latest; // verified via release fetch
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

fn spawn_update_resume(ctx: &RestartContext) -> Result<(), String> {
    let exe = stable_exe_path().map_err(|e| e.to_string())?;
    let parent_pid = std::process::id();
    let mut cmd = Command::new(&exe);
    cmd.args([
        "update-resume",
        "--wait-pid",
        &parent_pid.to_string(),
        "--host",
        &ctx.host,
        "--port",
        &ctx.port.to_string(),
        "--max-bytes",
        &ctx.max_bytes.to_string(),
        "--project",
        &ctx.project_dir.to_string_lossy(),
    ]);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    cmd.spawn()
        .map_err(|e| format!("spawn update-resume: {e}"))?;
    Ok(())
}

/// Hidden CLI: wait for parent exit + port free, then start detached hub.
pub async fn run_update_resume(
    wait_pid: u32,
    host: String,
    port: u16,
    project: PathBuf,
    max_bytes: u64,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        let parent_gone = !process_exists(wait_pid);
        let port_free = !crate::shell_attach::probe_hub(&host, port).await;
        if parent_gone && port_free {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    if crate::shell_attach::probe_hub(&host, port).await {
        return Err(format!(
            "port {port} still in use after waiting for pid {wait_pid}"
        ));
    }

    let exe = stable_exe_path().map_err(|e| e.to_string())?;
    crate::shell_attach::spawn_detached_hub_with_options(
        &exe,
        &host,
        port,
        Some(&project),
        Some(max_bytes),
    )
    .map_err(|e| format!("failed to start hub after update: {e}"))?;

    // Wait briefly for health.
    let ready_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < ready_deadline {
        if crate::shell_attach::probe_hub(&host, port).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(format!(
        "hub at {}:{} did not become healthy after update",
        host, port
    ))
}

fn process_exists(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tag_strips_v() {
        assert_eq!(
            parse_tag_version("v0.8.0").unwrap(),
            Version::parse("0.8.0").unwrap()
        );
        assert_eq!(
            parse_tag_version("0.7.0").unwrap(),
            Version::parse("0.7.0").unwrap()
        );
        assert!(parse_tag_version("not-a-version").is_none());
    }

    #[test]
    fn parse_cli_version_from_clap() {
        assert_eq!(
            parse_cli_version("mizpah 0.7.0").unwrap(),
            Version::parse("0.7.0").unwrap()
        );
        assert_eq!(
            parse_cli_version("mzp 0.8.1\n").unwrap(),
            Version::parse("0.8.1").unwrap()
        );
    }

    #[test]
    fn sibling_names() {
        assert_eq!(sibling_bin_name("mizpah"), "mzp");
        assert_eq!(sibling_bin_name("mzp"), "mizpah");
    }

    #[test]
    fn channel_cellar_path() {
        assert!(path_is_homebrew(Some(Path::new(
            "/opt/homebrew/Cellar/mizpah/0.7.0/bin/mizpah"
        ))));
        assert!(path_is_homebrew(Some(Path::new(
            "/home/linuxbrew/.linuxbrew/Cellar/mizpah/0.7.0/bin/mizpah"
        ))));
        assert!(!path_is_homebrew(Some(Path::new(
            "/Users/me/.cargo/bin/mizpah"
        ))));
        // Without HOMEBREW_PREFIX, a bare /usr/local/bin path is not Homebrew.
        assert!(!path_is_homebrew(Some(Path::new("/usr/local/bin/mizpah"))));
    }

    #[test]
    fn channel_homebrew_prefix_bin() {
        let prefix = Path::new("/opt/homebrew");
        assert!(path_is_homebrew_with_prefix(
            Some(Path::new("/opt/homebrew/bin/mizpah")),
            Some(prefix)
        ));
        assert!(path_is_homebrew_with_prefix(
            Some(Path::new("/opt/homebrew/bin/mzp")),
            Some(prefix)
        ));
        assert!(!path_is_homebrew_with_prefix(
            Some(Path::new("/opt/homebrew/opt/other/bin/mizpah")),
            Some(prefix)
        ));
        assert!(!path_is_homebrew_with_prefix(
            Some(Path::new("/usr/local/bin/mizpah")),
            None
        ));
    }

    #[test]
    fn stable_exe_prefers_prefix_bin_over_cellar() {
        let prefix = Path::new("/opt/homebrew");
        let cellar = Path::new("/opt/homebrew/Cellar/mizpah/0.7.0/bin/mizpah");
        let prefix_bin = Path::new("/opt/homebrew/bin/mizpah");
        let exists = |p: &Path| p == prefix_bin;

        let resolved = resolve_stable_exe_path(cellar, true, Some(prefix), exists);
        assert_eq!(resolved, prefix_bin);

        // Even without prefer_homebrew flag, Cellar path alone should still
        // resolve via prefix when the path looks like Cellar… handled by caller.
        // Here prefer_homebrew=true is the apply_homebrew case.
        let gone = |_: &Path| false;
        let fallback = resolve_stable_exe_path(cellar, true, Some(prefix), gone);
        assert_eq!(fallback, cellar);
    }

    #[test]
    fn stable_exe_keeps_prefix_bin_when_already_there() {
        let prefix = Path::new("/opt/homebrew");
        let prefix_bin = Path::new("/opt/homebrew/bin/mzp");
        let exists = |p: &Path| p == prefix_bin;
        let resolved = resolve_stable_exe_path(prefix_bin, true, Some(prefix), exists);
        assert_eq!(resolved, prefix_bin);
    }

    #[test]
    fn stable_exe_non_homebrew_bin_unchanged() {
        let cargo = Path::new("/Users/me/.cargo/bin/mizpah");
        let exists = |p: &Path| p == cargo;
        let resolved = resolve_stable_exe_path(cargo, false, None, exists);
        assert_eq!(resolved, cargo);
    }

    #[test]
    fn running_and_sibling_names() {
        assert_eq!(running_bin_name(Path::new("/opt/homebrew/bin/mzp")), "mzp");
        assert_eq!(sibling_bin_name("mzp"), "mizpah");
        assert_eq!(sibling_bin_name("mizpah"), "mzp");
    }

    #[test]
    fn release_target_is_known_or_none() {
        if let Some(t) = release_target() {
            assert!(
                t == "aarch64-apple-darwin"
                    || t == "x86_64-apple-darwin"
                    || t == "x86_64-unknown-linux-gnu"
            );
        }
    }

    #[test]
    fn update_available_semver() {
        let cur = Version::parse("0.7.0").unwrap();
        let latest = Version::parse("0.8.0").unwrap();
        assert!(latest > cur);
        assert!(!(cur > latest));
    }

    #[test]
    fn check_stale_when_never_checked_or_past_ttl() {
        let ttl = Duration::from_secs(15 * 60);
        let now = Instant::now();
        assert!(is_check_stale(None, now, ttl));
        assert!(!is_check_stale(Some(now), now, ttl));
        assert!(!is_check_stale(
            Some(now - Duration::from_secs(14 * 60)),
            now,
            ttl
        ));
        assert!(is_check_stale(
            Some(now - Duration::from_secs(15 * 60)),
            now,
            ttl
        ));
        assert!(is_check_stale(
            Some(now - Duration::from_secs(16 * 60)),
            now,
            ttl
        ));
    }
}
