//! Check GitHub Releases for updates and apply Homebrew or direct self-updates.

mod apply;
mod check;
mod resume;

pub use apply::apply_update;
#[allow(unused_imports)]
pub use check::{
    detect_channel, fetch_latest_release, find_brew_binary, parse_cli_version, parse_tag_version,
    release_target, resolve_stable_exe_path, running_bin_name, sibling_bin_name, stable_exe_path,
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
pub(crate) const BREW_FORMULA: &str = "ethira-dev/mizpah/mizpah";

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
    pub ttl_hours: u64,
}

pub(crate) struct Inner {
    pub(crate) installed_version: Version,
    pub(crate) latest_version: Option<Version>,
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
