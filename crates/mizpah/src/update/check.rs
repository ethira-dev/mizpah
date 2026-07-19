//! GitHub release checks and install channel detection.

use super::{UpdateChannel, CHECK_TIMEOUT, GITHUB_REPO};
use semver::Version;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    pub version: Version,
    pub download_url: Option<String>,
    /// GitHub release body (changelog / release notes). Empty bodies are `None`.
    pub body: Option<String>,
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

pub(crate) fn is_check_stale(
    last_checked_at: Option<Instant>,
    now: Instant,
    ttl: Duration,
) -> bool {
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

pub fn path_is_homebrew(path: Option<&Path>) -> bool {
    path_is_homebrew_with_prefix(path, homebrew_prefix_from_env_only().as_deref())
}

pub fn path_is_homebrew_with_prefix(path: Option<&Path>, prefix: Option<&Path>) -> bool {
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
pub fn stable_exe_path() -> std::io::Result<PathBuf> {
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
pub fn resolve_stable_exe_path(
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

pub fn path_looks_like_homebrew_cellar(path: &Path) -> bool {
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

pub fn homebrew_prefix() -> Option<PathBuf> {
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
    crate::util::which(name)
}

#[derive(serde::Deserialize)]
pub struct GhAsset {
    pub name: String,
    pub browser_download_url: String,
}

#[derive(serde::Deserialize)]
pub struct GhRelease {
    pub tag_name: String,
    pub assets: Vec<GhAsset>,
    #[serde(default)]
    pub body: Option<String>,
}

fn normalize_release_body(body: Option<String>) -> Option<String> {
    body.and_then(|b| {
        let trimmed = b.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

pub fn parse_github_release(body: &str) -> Result<ReleaseInfo, String> {
    let gh_release: GhRelease = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let version = parse_tag_version(&gh_release.tag_name)
        .ok_or_else(|| format!("invalid release tag {}", gh_release.tag_name))?;

    let download_url = release_target().and_then(|target| {
        let want = format!("mizpah-{target}.tar.gz");
        gh_release
            .assets
            .into_iter()
            .find(|a| a.name == want)
            .map(|a| a.browser_download_url)
    });

    Ok(ReleaseInfo {
        version,
        download_url,
        body: normalize_release_body(gh_release.body),
    })
}

pub async fn fetch_latest_release() -> Result<ReleaseInfo, String> {
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    fetch_release_at(&url).await
}

pub(crate) async fn fetch_release_at(url: &str) -> Result<ReleaseInfo, String> {
    crate::util::ensure_rustls_crypto_provider();
    let client = reqwest::Client::builder()
        .timeout(CHECK_TIMEOUT)
        .user_agent(format!("mizpah/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;

    let resp = client
        .get(url)
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

    let body = resp.text().await.map_err(|e| e.to_string())?;
    parse_github_release(&body)
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

    #[test]
    fn parse_github_release_valid() {
        // Extra # hashes: JSON value `"## …` would terminate r##"…"##.
        let json = r###"{
            "tag_name": "v0.8.0",
            "body": "## What's Changed\n* Fix foo by @dev\n",
            "assets": [
                {
                    "name": "mizpah-aarch64-apple-darwin.tar.gz",
                    "browser_download_url": "https://github.com/ethira-dev/mizpah/releases/download/v0.8.0/mizpah-aarch64-apple-darwin.tar.gz"
                },
                {
                    "name": "mizpah-x86_64-unknown-linux-gnu.tar.gz",
                    "browser_download_url": "https://github.com/ethira-dev/mizpah/releases/download/v0.8.0/mizpah-x86_64-unknown-linux-gnu.tar.gz"
                }
            ]
        }"###;
        
        let info = parse_github_release(json).unwrap();
        assert_eq!(info.version, Version::parse("0.8.0").unwrap());
        assert_eq!(
            info.body.as_deref(),
            Some("## What's Changed\n* Fix foo by @dev")
        );
        if let Some(url) = info.download_url {
            assert!(url.contains("mizpah-"));
            assert!(url.contains(".tar.gz"));
        }
    }

    #[test]
    fn parse_github_release_empty_body_is_none() {
        let json = r#"{
            "tag_name": "v0.8.0",
            "body": "   \n\t  ",
            "assets": []
        }"#;
        let info = parse_github_release(json).unwrap();
        assert!(info.body.is_none());
    }

    #[test]
    fn parse_github_release_missing_body_is_none() {
        let json = r#"{
            "tag_name": "v0.8.0",
            "assets": []
        }"#;
        let info = parse_github_release(json).unwrap();
        assert!(info.body.is_none());
    }

    #[test]
    fn parse_github_release_invalid_tag() {
        let json = r#"{
            "tag_name": "not-a-version",
            "assets": []
        }"#;
        
        let result = parse_github_release(json);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid release tag"));
    }

    #[test]
    fn parse_github_release_malformed_json() {
        let json = r#"{ invalid json"#;
        
        let result = parse_github_release(json);
        assert!(result.is_err());
    }

    #[test]
    fn parse_github_release_missing_asset() {
        let json = r#"{
            "tag_name": "v0.8.0",
            "assets": [
                {
                    "name": "other-file.txt",
                    "browser_download_url": "https://example.com/file.txt"
                }
            ]
        }"#;
        
        let info = parse_github_release(json).unwrap();
        assert_eq!(info.version, Version::parse("0.8.0").unwrap());
        assert!(info.download_url.is_none());
    }

    #[test]
    fn parse_github_release_no_assets() {
        let json = r#"{
            "tag_name": "v0.8.0",
            "assets": []
        }"#;
        
        let info = parse_github_release(json).unwrap();
        assert_eq!(info.version, Version::parse("0.8.0").unwrap());
        assert!(info.download_url.is_none());
    }

    #[test]
    fn path_is_homebrew_none_and_cellar_prefix() {
        assert!(!path_is_homebrew(None));
        let prefix = Path::new("/opt/homebrew");
        assert!(path_is_homebrew_with_prefix(
            Some(Path::new("/opt/homebrew/Cellar/mizpah/1.0/bin/mizpah")),
            Some(prefix)
        ));
    }

    #[test]
    fn path_looks_like_homebrew_cellar_detects() {
        assert!(path_looks_like_homebrew_cellar(Path::new(
            "/opt/homebrew/Cellar/mizpah/0.7.0/bin/mizpah"
        )));
        assert!(!path_looks_like_homebrew_cellar(Path::new("/usr/local/bin/mizpah")));
    }

    #[test]
    fn parse_cli_version_none_when_missing() {
        assert!(parse_cli_version("no version here").is_none());
    }

    #[test]
    fn resolve_stable_exe_path_prefix_bin_without_prefer_homebrew() {
        let prefix = Path::new("/opt/homebrew");
        let prefix_bin = Path::new("/opt/homebrew/bin/mzp");
        let exists = |p: &Path| p == prefix_bin;
        let resolved = resolve_stable_exe_path(prefix_bin, false, Some(prefix), exists);
        assert_eq!(resolved, prefix_bin);
    }

    #[test]
    fn resolve_stable_exe_path_generic_bin_parent() {
        let cargo = Path::new("/Users/me/.cargo/bin/mizpah");
        let exists = |p: &Path| p == cargo;
        let resolved = resolve_stable_exe_path(cargo, false, None, exists);
        assert_eq!(resolved, cargo);
    }

    #[test]
    fn running_bin_name_fallback() {
        assert_eq!(running_bin_name(Path::new("/opt/homebrew/bin/mzp")), "mzp");
    }

    #[test]
    fn homebrew_prefix_from_env() {
        let _guard = crate::test_support::env_lock();
        let old = std::env::var_os("HOMEBREW_PREFIX");
        std::env::set_var("HOMEBREW_PREFIX", "/tmp/homebrew-test-prefix");
        assert_eq!(
            homebrew_prefix().as_deref(),
            Some(Path::new("/tmp/homebrew-test-prefix"))
        );
        match old {
            Some(v) => std::env::set_var("HOMEBREW_PREFIX", v),
            None => std::env::remove_var("HOMEBREW_PREFIX"),
        }
    }

    #[test]
    fn find_brew_binary_smoke() {
        let _ = find_brew_binary();
    }

    #[test]
    fn detect_channel_and_stable_exe_smoke() {
        let _channel = detect_channel();
        let _ = stable_exe_path();
    }

    #[cfg(not(miri))]
    mod fetch_http {
        use super::*;
        use axum::body::Body;
        use axum::http::{Response, StatusCode};
        use axum::routing::get;
        use axum::Router;
        use tokio::net::TcpListener;

        async fn start_mock(body: &'static str, status: StatusCode) -> String {
            let app = Router::new().route(
                "/releases/latest",
                get(move || {
                    let body = body;
                    async move {
                        Response::builder()
                            .status(status)
                            .body(Body::from(body))
                            .unwrap()
                    }
                }),
            );
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            tokio::task::yield_now().await;
            format!("http://{addr}/releases/latest")
        }

        #[tokio::test]
        async fn fetch_release_at_success() {
            let json = r#"{
                "tag_name": "v0.8.0",
                "assets": [
                    {
                        "name": "mizpah-aarch64-apple-darwin.tar.gz",
                        "browser_download_url": "https://example.com/mizpah.tar.gz"
                    }
                ]
            }"#;
            let url = start_mock(json, StatusCode::OK).await;
            let info = fetch_release_at(&url).await.unwrap();
            assert_eq!(info.version, Version::parse("0.8.0").unwrap());
        }

        #[tokio::test]
        async fn fetch_release_at_not_found() {
            let url = start_mock("", StatusCode::NOT_FOUND).await;
            let err = fetch_release_at(&url).await.unwrap_err();
            assert_eq!(err, "no latest release");
        }

        #[tokio::test]
        async fn fetch_release_at_rate_limited() {
            let url = start_mock("", StatusCode::FORBIDDEN).await;
            let err = fetch_release_at(&url).await.unwrap_err();
            assert!(err.contains("rate limited"));
        }

        #[tokio::test]
        async fn fetch_release_at_server_error() {
            let url = start_mock("", StatusCode::INTERNAL_SERVER_ERROR).await;
            let err = fetch_release_at(&url).await.unwrap_err();
            assert!(err.contains("GitHub API"));
        }
    }
}
