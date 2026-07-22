//! Check GitHub Releases for updates and apply Homebrew or direct self-updates.

mod apply;
mod check;
mod resume;

pub use apply::{apply_update, ApplyOutcome};
#[allow(unused_imports)]
pub use check::{
    detect_channel, fetch_latest_release, find_brew_binary, parse_cli_version, parse_tag_version,
    release_target, resolve_stable_exe_path, running_bin_name, sibling_bin_name, stable_exe_path,
    ReleaseInfo,
};
pub use resume::run_update_resume;

use semver::Version;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex};
use tracing::debug;

pub(crate) const GITHUB_REPO: &str = "ethira-dev/mizpah";
pub(crate) const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// Re-fetch GitHub latest when status is read and the cache is older than this.
pub(crate) const CHECK_TTL: Duration = Duration::from_secs(15 * 60);
pub(crate) const BREW_FORMULA: &str = "ethira-dev/tap/mizpah";

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_notes: Option<String>,
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
    pub ttl_hours: u64,
}

pub(crate) struct Inner {
    pub(crate) installed_version: Version,
    pub(crate) latest_version: Option<Version>,
    pub(crate) latest_release_notes: Option<String>,
    pub(crate) channel: UpdateChannel,
    pub(crate) busy: bool,
    pub(crate) last_checked_at: Option<Instant>,
}

pub struct UpdateManager {
    pub(crate) inner: Mutex<Inner>,
    pub(crate) restart: RestartContext,
}

impl UpdateManager {
    pub fn new(restart: RestartContext) -> Arc<Self> {
        let current =
            Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 0, 0));
        Arc::new(Self {
            inner: Mutex::new(Inner {
                installed_version: current,
                latest_version: None,
                latest_release_notes: None,
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
            .is_some_and(|l| l > &g.installed_version);
        UpdateStatus {
            installed_version: g.installed_version.to_string(),
            latest_version: g.latest_version.as_ref().map(|v| v.to_string()),
            release_notes: g.latest_release_notes.clone(),
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
            check::is_check_stale(g.last_checked_at, Instant::now(), CHECK_TTL)
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
                g.latest_release_notes = info.body;
                g.channel = detect_channel();
                g.last_checked_at = Some(Instant::now());
            }
            Err(err) => {
                debug!(error = %err, "update check failed");
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

pub type ProgressTx = mpsc::UnboundedSender<UpdateEvent>;

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_manager() -> Arc<UpdateManager> {
        UpdateManager::new(RestartContext {
            host: "127.0.0.1".into(),
            port: 3149,
            project_dir: std::env::temp_dir(),
            max_bytes: 1024 * 1024,
            ttl_hours: 24,
        })
    }

    #[tokio::test]
    async fn new_manager_has_current_version() {
        let manager = make_test_manager();
        let status = manager.status().await;
        assert!(!status.installed_version.is_empty());
        assert_eq!(status.channel, detect_channel());
        assert!(!status.busy);
    }

    #[tokio::test]
    async fn status_checks_for_updates_when_stale() {
        let manager = make_test_manager();

        {
            let mut g = manager.inner.lock().await;
            g.last_checked_at = None;
        }

        let _status = manager.status().await;

        let g = manager.inner.lock().await;
        assert!(g.last_checked_at.is_some());
    }

    #[tokio::test]
    async fn status_skips_check_when_fresh() {
        let manager = make_test_manager();

        {
            let mut g = manager.inner.lock().await;
            g.last_checked_at = Some(Instant::now());
        }

        let _status = manager.status().await;
    }

    #[tokio::test]
    async fn status_skips_check_when_busy() {
        let manager = make_test_manager();

        {
            let mut g = manager.inner.lock().await;
            g.busy = true;
            g.last_checked_at = None;
        }

        let _status = manager.status().await;

        let g = manager.inner.lock().await;
        assert!(g.last_checked_at.is_none());
    }

    #[tokio::test]
    async fn try_begin_apply_when_no_update() {
        let manager = make_test_manager();

        {
            let mut g = manager.inner.lock().await;
            g.latest_version = None;
        }

        let result = manager.try_begin_apply().await;
        assert!(matches!(result, Err(ApplyBeginError::NoUpdate)));
    }

    #[tokio::test]
    async fn try_begin_apply_when_already_latest() {
        let manager = make_test_manager();

        {
            let mut g = manager.inner.lock().await;
            g.installed_version = Version::new(1, 0, 0);
            g.latest_version = Some(Version::new(1, 0, 0));
        }

        let result = manager.try_begin_apply().await;
        assert!(matches!(result, Err(ApplyBeginError::NoUpdate)));
    }

    #[tokio::test]
    async fn try_begin_apply_when_busy() {
        let manager = make_test_manager();

        {
            let mut g = manager.inner.lock().await;
            g.busy = true;
            g.installed_version = Version::new(1, 0, 0);
            g.latest_version = Some(Version::new(1, 1, 0));
        }

        let result = manager.try_begin_apply().await;
        assert!(matches!(result, Err(ApplyBeginError::Busy)));
    }

    #[tokio::test]
    async fn try_begin_apply_success() {
        let manager = make_test_manager();

        {
            let mut g = manager.inner.lock().await;
            g.installed_version = Version::new(1, 0, 0);
            g.latest_version = Some(Version::new(1, 1, 0));
            g.busy = false;
        }

        let result = manager.try_begin_apply().await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Version::new(1, 1, 0));

        let g = manager.inner.lock().await;
        assert!(g.busy);
    }

    #[tokio::test]
    async fn clear_busy() {
        let manager = make_test_manager();

        {
            let mut g = manager.inner.lock().await;
            g.busy = true;
        }

        manager.clear_busy().await;

        let g = manager.inner.lock().await;
        assert!(!g.busy);
    }

    #[tokio::test]
    async fn ensure_fresh_when_never_checked() {
        let manager = make_test_manager();

        {
            let mut g = manager.inner.lock().await;
            g.last_checked_at = None;
        }

        manager.ensure_fresh().await;

        let g = manager.inner.lock().await;
        assert!(g.last_checked_at.is_some());
    }

    #[tokio::test]
    async fn ensure_fresh_when_stale() {
        let manager = make_test_manager();

        {
            let mut g = manager.inner.lock().await;
            g.last_checked_at = Some(Instant::now() - CHECK_TTL - Duration::from_secs(1));
        }

        manager.ensure_fresh().await;

        let g = manager.inner.lock().await;
        let elapsed = Instant::now().saturating_duration_since(g.last_checked_at.unwrap());
        assert!(elapsed < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn check_now_updates_timestamp() {
        let manager = make_test_manager();

        {
            let mut g = manager.inner.lock().await;
            g.last_checked_at = None;
        }

        manager.check_now().await;

        let g = manager.inner.lock().await;
        assert!(g.last_checked_at.is_some());
    }

    #[tokio::test]
    async fn check_now_skips_when_busy() {
        let manager = make_test_manager();

        {
            let mut g = manager.inner.lock().await;
            g.busy = true;
            g.last_checked_at = None;
        }

        manager.check_now().await;

        let g = manager.inner.lock().await;
        assert!(g.last_checked_at.is_none());
    }

    #[tokio::test]
    async fn restart_context_returns_ref() {
        let manager = make_test_manager();
        let ctx = manager.restart_context();
        assert_eq!(ctx.host, "127.0.0.1");
        assert_eq!(ctx.port, 3149);
    }

    #[test]
    fn update_channel_serialization() {
        let homebrew = UpdateChannel::Homebrew;
        let direct = UpdateChannel::Direct;

        let json_hb = serde_json::to_string(&homebrew).unwrap();
        let json_dir = serde_json::to_string(&direct).unwrap();

        assert_eq!(json_hb, r#""homebrew""#);
        assert_eq!(json_dir, r#""direct""#);
    }

    #[test]
    fn update_status_serialization() {
        let status = UpdateStatus {
            installed_version: "1.0.0".into(),
            latest_version: Some("1.1.0".into()),
            release_notes: Some("What's Changed\n* Fix foo".into()),
            update_available: true,
            channel: UpdateChannel::Direct,
            busy: false,
        };

        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("installedVersion"));
        assert!(json.contains("latestVersion"));
        assert!(json.contains("releaseNotes"));
        assert!(json.contains("Fix foo"));
        assert!(json.contains("updateAvailable"));
        assert!(json.contains("channel"));
        assert!(json.contains("busy"));
    }

    #[test]
    fn update_event_serialization() {
        let event = UpdateEvent {
            step: "Testing".into(),
            progress: 0.5,
            error: Some("test error".into()),
            restarting: Some(true),
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("step"));
        assert!(json.contains("progress"));
        assert!(json.contains("error"));
        assert!(json.contains("restarting"));
    }

    #[tokio::test]
    async fn ensure_fresh_skips_when_cache_fresh() {
        let manager = make_test_manager();
        let checked = Instant::now();
        {
            let mut g = manager.inner.lock().await;
            g.last_checked_at = Some(checked);
        }
        manager.ensure_fresh().await;
        let g = manager.inner.lock().await;
        assert_eq!(g.last_checked_at, Some(checked));
    }

    #[test]
    fn apply_begin_error_variants() {
        let busy = ApplyBeginError::Busy;
        let no_update = ApplyBeginError::NoUpdate;

        assert!(matches!(busy, ApplyBeginError::Busy));
        assert!(matches!(no_update, ApplyBeginError::NoUpdate));
    }
}
