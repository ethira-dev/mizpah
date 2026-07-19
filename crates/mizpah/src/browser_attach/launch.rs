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
    wait_for_cdp_until(cdp_port, Duration::from_secs(15)).await
}

pub(crate) async fn wait_for_cdp_until(cdp_port: u16, timeout: Duration) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
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
    let args = browser_launch_args(cdp_port, &profile);
    let mut cmd = std::process::Command::new(&binary);
    cmd.args(&args)
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

pub(crate) fn browser_launch_args(cdp_port: u16, profile: &std::path::Path) -> Vec<String> {
    vec![
        format!("--remote-debugging-port={cdp_port}"),
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "about:blank".into(),
    ]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_browser_returns_some_or_none() {
        let result = find_browser_binary();
        if let Some(path) = result {
            assert!(path.to_str().is_some());
        }
    }

    #[tokio::test]
    async fn resolve_cdp_ws_url_with_explicit_url() {
        let opts = BrowserAttachOpts {
            cdp_url: Some("ws://localhost:9222/devtools/browser/abc".into()),
            ..Default::default()
        };
        let url = resolve_cdp_ws_url(&opts).await.unwrap();
        assert_eq!(url, "ws://localhost:9222/devtools/browser/abc");
    }

    #[tokio::test]
    async fn resolve_cdp_ws_url_rejects_empty() {
        let opts = BrowserAttachOpts {
            cdp_url: Some("  ".into()),
            ..Default::default()
        };
        let result = resolve_cdp_ws_url(&opts).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must not be empty"));
    }

    #[tokio::test]
    async fn resolve_cdp_ws_url_for_reconnect_with_url() {
        let url = resolve_cdp_ws_url_for_reconnect(9222, Some("ws://test")).await;
        assert_eq!(url.unwrap(), "ws://test");
    }

    #[tokio::test]
    async fn resolve_cdp_ws_url_for_reconnect_ignores_empty() {
        let url = resolve_cdp_ws_url_for_reconnect(9999, Some("  ")).await;
        assert!(url.is_err());
    }

    #[test]
    fn chrome_profile_dir_returns_path() {
        let dir = chrome_profile_dir();
        assert!(dir.is_ok());
        let path = dir.unwrap();
        assert!(path.to_str().unwrap().contains("chrome-profile"));
    }

    #[tokio::test]
    async fn wait_for_cdp_timeout() {
        let result = wait_for_cdp_until(19999, Duration::from_millis(400)).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("timed out"));
    }

    #[tokio::test]
    async fn fetch_browser_ws_url_not_reachable() {
        let result = fetch_browser_ws_url(19998).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("cannot reach") || err.contains("Chrome"));
    }

    #[test]
    fn browser_launch_args_include_port_and_profile() {
        let args = browser_launch_args(9333, std::path::Path::new("/tmp/prof"));
        assert!(args.iter().any(|a| a.contains("9333")));
        assert!(args.iter().any(|a| a.contains("/tmp/prof")));
        assert!(args.iter().any(|a| a == "about:blank"));
    }

    /// Minimal HTTP server answering Chrome `/json/version`.
    async fn serve_cdp_version(body: &str) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = body.to_string();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        });
        port
    }

    #[tokio::test]
    async fn fetch_browser_ws_url_success() {
        let port = serve_cdp_version(
            r#"{"webSocketDebuggerUrl":"ws://127.0.0.1:1/devtools/browser/x"}"#,
        )
        .await;
        let url = fetch_browser_ws_url(port).await.unwrap();
        assert!(url.contains("devtools/browser"));
    }

    #[tokio::test]
    async fn fetch_browser_ws_url_missing_field() {
        let port = serve_cdp_version(r#"{"Browser":"Chrome"}"#).await;
        let err = fetch_browser_ws_url(port).await.unwrap_err();
        assert!(err.contains("missing webSocketDebuggerUrl"));
    }

    #[tokio::test]
    async fn fetch_browser_ws_url_http_error_status() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = sock.read(&mut buf).await;
            let resp = "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = sock.write_all(resp.as_bytes()).await;
        });
        let err = fetch_browser_ws_url(port).await.unwrap_err();
        assert!(err.contains("returned"));
    }

    #[tokio::test]
    async fn wait_for_cdp_succeeds_when_ready() {
        let port = serve_cdp_version(
            r#"{"webSocketDebuggerUrl":"ws://127.0.0.1:1/devtools/browser/x"}"#,
        )
        .await;
        wait_for_cdp_until(port, Duration::from_secs(2))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn resolve_cdp_ws_url_fetches_when_no_override() {
        let port = serve_cdp_version(
            r#"{"webSocketDebuggerUrl":"ws://127.0.0.1:1/devtools/browser/y"}"#,
        )
        .await;
        let opts = BrowserAttachOpts {
            cdp_port: port,
            cdp_url: None,
            ..Default::default()
        };
        let url = resolve_cdp_ws_url(&opts).await.unwrap();
        assert!(url.contains("browser/y"));
    }
}
