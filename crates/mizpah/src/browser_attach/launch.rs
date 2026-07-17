//! Chrome/Edge launch and CDP endpoint discovery.

use super::BrowserAttachOpts;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tracing::info;

pub(crate) const DEFAULT_CDP_PORT: u16 = 9222;
const HTTP_TIMEOUT: Duration = Duration::from_secs(3);

pub(crate) async fn resolve_cdp_ws_url(opts: &BrowserAttachOpts) -> Result<String, String> {
    if let Some(ref url) = opts.cdp_url {
        let t = url.trim();
        if t.is_empty() {
            return Err("--cdp-url must not be empty".into());
        }
        return Ok(t.to_string());
    }
    fetch_browser_ws_url(opts.cdp_port).await
}

pub(crate) async fn resolve_cdp_ws_url_for_reconnect(
    cdp_port: u16,
    cdp_url: Option<&str>,
) -> Result<String, String> {
    if let Some(url) = cdp_url {
        let t = url.trim();
        if !t.is_empty() {
            return Ok(t.to_string());
        }
    }
    fetch_browser_ws_url(cdp_port).await
}

pub(crate) async fn fetch_browser_ws_url(cdp_port: u16) -> Result<String, String> {
    crate::util::ensure_rustls_crypto_provider();
    let version_url = format!("http://127.0.0.1:{cdp_port}/json/version");
    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(&version_url).send().await.map_err(|e| {
        format!(
            "cannot reach Chrome DevTools at {version_url}: {e}\n\
             Start Chrome with --remote-debugging-port={cdp_port}, or use `mzp attach browser --launch`"
        )
    })?;
    if !resp.status().is_success() {
        return Err(format!(
            "Chrome DevTools at {version_url} returned {}",
            resp.status()
        ));
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    body.get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "Chrome /json/version missing webSocketDebuggerUrl".into())
}

pub(crate) async fn wait_for_cdp(cdp_port: u16) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut last_err = String::new();
    while tokio::time::Instant::now() < deadline {
        match fetch_browser_ws_url(cdp_port).await {
            Ok(_) => return Ok(()),
            Err(e) => last_err = e,
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    Err(format!(
        "timed out waiting for CDP on :{cdp_port}: {last_err}"
    ))
}

pub(crate) fn launch_browser(cdp_port: u16) -> Result<std::process::Child, String> {
    let binary = find_browser_binary().ok_or_else(|| {
        "could not find Google Chrome or Microsoft Edge; install one or pass --cdp-url".to_string()
    })?;
    let profile = chrome_profile_dir()?;
    std::fs::create_dir_all(&profile)
        .map_err(|e| format!("failed to create chrome profile dir: {e}"))?;

    let mut cmd = std::process::Command::new(&binary);
    cmd.arg(format!("--remote-debugging-port={cdp_port}"))
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    crate::unix_process::apply_pre_exec_setsid(&mut cmd);

    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to launch {}: {e}", binary.display()))?;
    info!(
        binary = %binary.display(),
        profile = %profile.display(),
        cdp_port,
        "browser attach: launched browser (dedicated profile)"
    );
    Ok(child)
}

fn chrome_profile_dir() -> Result<PathBuf, String> {
    let dir = crate::util::config_dir().map_err(|e| e.to_string())?;
    Ok(dir.join("chrome-profile"))
}

fn find_browser_binary() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let candidates = [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
        ];
        candidates
            .into_iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
    }
    #[cfg(target_os = "linux")]
    {
        for name in [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
            "microsoft-edge",
        ] {
            if let Some(p) = which_bin(name) {
                return Some(p);
            }
        }
        None
    }
    #[cfg(target_os = "windows")]
    {
        let mut candidates = Vec::new();
        if let Ok(pf) = std::env::var("PROGRAMFILES") {
            candidates.push(PathBuf::from(&pf).join("Google\\Chrome\\Application\\chrome.exe"));
            candidates.push(PathBuf::from(&pf).join("Microsoft\\Edge\\Application\\msedge.exe"));
        }
        if let Ok(pf86) = std::env::var("PROGRAMFILES(X86)") {
            candidates.push(PathBuf::from(&pf86).join("Google\\Chrome\\Application\\chrome.exe"));
            candidates.push(PathBuf::from(&pf86).join("Microsoft\\Edge\\Application\\msedge.exe"));
        }
        candidates.into_iter().find(|p| p.is_file())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
fn which_bin(name: &str) -> Option<PathBuf> {
    crate::util::which(name)
}
