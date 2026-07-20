//! Local (and optional SSH) file ingest into a hub (Phase D + L).

use crate::config::MizpahConfig;
use crate::file_convert::{self, is_convertible_path};
use crate::ingest_forward::{http_client, BatchError, BATCH_MAX};
use crate::mzp_meta::MzpMeta;
use bzip2::read::BzDecoder;
use flate2::read::GzDecoder;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// True when path looks like `user@host:path` remote notation.
pub fn is_remote_path(path: &str) -> bool {
    // Avoid matching Windows drive letters (C:\…)
    if let Some(rest) = path.split_once(':') {
        let host = rest.0;
        if host.len() == 1 && host.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            return false;
        }
        return host.contains('@') || (!host.contains('/') && !host.contains('\\'));
    }
    false
}

fn secure_mode() -> bool {
    if let Ok(v) = std::env::var("MIZPAH_SECURE") {
        let v = v.trim();
        if v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes") {
            return true;
        }
        if v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("no") {
            return false;
        }
    }
    MizpahConfig::load().secure
}

fn is_compressed_path(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    name.ends_with(".gz") || name.ends_with(".bz2")
}

fn open_reader(path: &Path) -> Result<Box<dyn BufRead + Send>, IngestError> {
    let file = File::open(path)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if name.ends_with(".gz") {
        Ok(Box::new(BufReader::new(GzDecoder::new(file))))
    } else if name.ends_with(".bz2") {
        Ok(Box::new(BufReader::new(BzDecoder::new(file))))
    } else {
        Ok(Box::new(BufReader::new(file)))
    }
}

fn file_len(path: &Path) -> std::io::Result<u64> {
    Ok(std::fs::metadata(path)?.len())
}

/// Read new bytes from `offset` to EOF as lines; returns (lines_ingested, new_offset).
/// If the file was truncated below `offset`, restarts from 0.
async fn ingest_from_offset(
    client: &reqwest::Client,
    url: &str,
    service: &str,
    mzp: &MzpMeta,
    path: &Path,
    offset: u64,
    format_hint: Option<&str>,
) -> Result<(usize, u64), IngestError> {
    let meta_len = file_len(path)?;
    let start = if meta_len < offset { 0 } else { offset };
    if start >= meta_len {
        return Ok((0, meta_len));
    }
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    let mut total = 0usize;
    let mut line = String::new();
    let mut bytes_read = 0u64;
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        bytes_read += n as u64;
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if !line.contains('\n') {
            // Incomplete trailing line (no newline yet) — wait for more data.
            bytes_read -= n as u64;
            break;
        }
        if trimmed.is_empty() {
            continue;
        }
        buf.push(trimmed.to_string());
        total += 1;
        if buf.len() >= BATCH_MAX {
            flush_lines(client, url, service, mzp, &mut buf, format_hint).await?;
        }
    }
    flush_lines(client, url, service, mzp, &mut buf, format_hint).await?;
    Ok((total, start + bytes_read))
}

fn expand_paths(patterns: &[String]) -> Result<Vec<PathBuf>, IngestError> {
    let mut out = Vec::new();
    for p in patterns {
        if is_remote_path(p) {
            out.push(PathBuf::from(p));
            continue;
        }
        // Simple glob: `*` in filename only
        let path = PathBuf::from(p);
        if let Some(parent) = path.parent() {
            let parent = if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            };
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(p.as_str());
            if name.contains('*') || name.contains('?') {
                let pattern = name.to_string();
                let entries = std::fs::read_dir(parent)
                    .map_err(|e| IngestError::Message(format!("read {}: {e}", parent.display())))?;
                for ent in entries.flatten() {
                    let fname = ent.file_name();
                    let fname = fname.to_string_lossy();
                    if glob_match(&pattern, &fname) {
                        out.push(ent.path());
                    }
                }
                continue;
            }
        }
        out.push(path);
    }
    out.sort();
    out.dedup();
    if out.is_empty() {
        return Err(IngestError::Message("no files matched".into()));
    }
    Ok(out)
}

fn glob_match(pattern: &str, name: &str) -> bool {
    // Minimal `*` / `?` matcher
    let pb: Vec<char> = pattern.chars().collect();
    let nb: Vec<char> = name.chars().collect();
    fn rec(p: &[char], n: &[char]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some('*'), _) => (0..=n.len()).any(|i| rec(&p[1..], &n[i..])),
            (Some('?'), Some(_)) => rec(&p[1..], &n[1..]),
            (Some(a), Some(b)) if a == b => rec(&p[1..], &n[1..]),
            _ => false,
        }
    }
    rec(&pb, &nb)
}

async fn flush_lines(
    client: &reqwest::Client,
    url: &str,
    service: &str,
    mzp: &MzpMeta,
    buf: &mut Vec<String>,
    format_hint: Option<&str>,
) -> Result<(), IngestError> {
    if buf.is_empty() {
        return Ok(());
    }
    match crate::ingest_forward::post_batch_hint(client, url, service, None, mzp, buf, format_hint)
        .await
    {
        Ok(()) => {
            buf.clear();
            Ok(())
        }
        Err(BatchError::Disconnected) => {
            Err(IngestError::Message("service disconnected on hub".into()))
        }
        Err(BatchError::Other(e)) => Err(IngestError::Message(e)),
    }
}

async fn ingest_converted_lines(
    client: &reqwest::Client,
    url: &str,
    service: &str,
    mzp: &MzpMeta,
    lines: &[String],
    format_hint: &str,
) -> Result<usize, IngestError> {
    let mut buf = Vec::new();
    let mut total = 0usize;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        buf.push(line.clone());
        total += 1;
        if buf.len() >= BATCH_MAX {
            flush_lines(client, url, service, mzp, &mut buf, Some(format_hint)).await?;
        }
    }
    flush_lines(client, url, service, mzp, &mut buf, Some(format_hint)).await?;
    Ok(total)
}

async fn ingest_reader<R: BufRead + Send>(
    client: &reqwest::Client,
    url: &str,
    service: &str,
    mzp: &MzpMeta,
    mut reader: R,
) -> Result<(usize, Option<String>), IngestError> {
    let mut buf = Vec::new();
    let mut probe: Vec<String> = Vec::new();
    let mut format_hint: Option<String> = None;
    let mut total = 0usize;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let owned = trimmed.to_string();
        if format_hint.is_none() && probe.len() < 256 {
            probe.push(owned.clone());
            if probe.len() == 256 {
                format_hint = crate::formats::suggest_format_lock(&probe);
            }
        }
        buf.push(owned);
        total += 1;
        if buf.len() >= BATCH_MAX {
            if format_hint.is_none() && !probe.is_empty() {
                format_hint = crate::formats::suggest_format_lock(&probe);
            }
            flush_lines(client, url, service, mzp, &mut buf, format_hint.as_deref()).await?;
        }
    }
    if format_hint.is_none() && !probe.is_empty() {
        format_hint = crate::formats::suggest_format_lock(&probe);
    }
    flush_lines(client, url, service, mzp, &mut buf, format_hint.as_deref()).await?;
    Ok((total, format_hint))
}

fn fetch_remote_via_ssh(spec: &str) -> Result<PathBuf, IngestError> {
    if secure_mode() {
        return Err(IngestError::Message(
            "remote ingest refused (MIZPAH_SECURE / config.secure)".into(),
        ));
    }
    #[cfg(test)]
    if let Some(hook) = test_remote_fetch::get() {
        return hook(spec);
    }
    fetch_remote_via_ssh_commands(spec, run_ssh_cat, run_scp)
}

type SshCatFn = fn(&str, &str) -> Result<std::process::Output, String>;
type ScpFn = fn(&str, &std::path::Path) -> Result<std::process::Output, String>;

fn run_ssh_cat(host: &str, remote_path: &str) -> Result<std::process::Output, String> {
    Command::new("ssh")
        .arg(host)
        .arg("cat")
        .arg(remote_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("ssh failed: {e}"))
}

fn run_scp(spec: &str, dest: &std::path::Path) -> Result<std::process::Output, String> {
    Command::new("scp")
        .arg(spec)
        .arg(dest)
        .output()
        .map_err(|e| format!("scp failed: {e}"))
}

fn fetch_remote_via_ssh_commands(
    spec: &str,
    ssh_cat: SshCatFn,
    scp: ScpFn,
) -> Result<PathBuf, IngestError> {
    let (host_part, remote_path) = spec
        .split_once(':')
        .ok_or_else(|| IngestError::Message(format!("invalid remote path: {spec}")))?;
    let tmp = tempfile::NamedTempFile::new()?;
    let tmp_path = tmp.into_temp_path();
    let status = ssh_cat(host_part, remote_path).map_err(IngestError::Message)?;
    if !status.status.success() {
        let scp_out = scp(spec, &tmp_path).map_err(IngestError::Message)?;
        if !scp_out.status.success() {
            let err = String::from_utf8_lossy(&status.stderr);
            return Err(IngestError::Message(format!(
                "remote fetch failed for {spec}: {err}"
            )));
        }
        return tmp_path
            .keep()
            .map_err(|e| IngestError::Message(e.error.to_string()));
    }
    std::fs::write(&tmp_path, &status.stdout)?;
    tmp_path
        .keep()
        .map_err(|e| IngestError::Message(e.error.to_string()))
}

#[cfg(test)]
mod test_remote_fetch {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    type Hook = fn(&str) -> Result<PathBuf, IngestError>;
    static HOOK: OnceLock<Mutex<Option<Hook>>> = OnceLock::new();

    fn cell() -> &'static Mutex<Option<Hook>> {
        HOOK.get_or_init(|| Mutex::new(None))
    }

    pub(super) fn get() -> Option<Hook> {
        cell().lock().ok().and_then(|g| *g)
    }

    pub(crate) fn set(hook: Option<Hook>) {
        *cell().lock().unwrap() = hook;
    }
}

/// Ingest files (or globs / remote paths) into the hub via HTTP batch ingest.
pub async fn run_ingest(
    paths: Vec<String>,
    service: String,
    follow: bool,
    hub_host: &str,
    hub_port: u16,
) -> Result<(), IngestError> {
    let client = http_client().map_err(IngestError::Message)?;
    let url = format!("http://{hub_host}:{hub_port}/api/ingest/batch");
    let mzp = MzpMeta::capture();
    let expanded = expand_paths(&paths)?;
    if follow
        && expanded
            .iter()
            .any(|p| is_remote_path(&p.to_string_lossy()))
    {
        return Err(IngestError::Message(
            "--follow is not supported for remote paths".into(),
        ));
    }
    if follow && expanded.iter().any(|p| is_convertible_path(p)) {
        return Err(IngestError::Message(
            "--follow is not supported for binary logs (EVTX/pcap/NetFlow); ingest completed files only"
                .into(),
        ));
    }
    let mut offsets: HashMap<PathBuf, u64> = HashMap::new();
    let mut format_hints: HashMap<PathBuf, Option<String>> = HashMap::new();

    for path in &expanded {
        let path_str = path.to_string_lossy();
        let local = if is_remote_path(&path_str) {
            fetch_remote_via_ssh(&path_str)?
        } else {
            path.clone()
        };
        let (n, hint) = if is_convertible_path(&local) {
            let converted = file_convert::convert_file(&local)?;
            let n = ingest_converted_lines(
                &client,
                &url,
                &service,
                &mzp,
                &converted.lines,
                converted.format_hint,
            )
            .await?;
            (n, Some(converted.format_hint.to_string()))
        } else {
            let reader = open_reader(&local)?;
            ingest_reader(&client, &url, &service, &mzp, reader).await?
        };
        eprintln!("ingested {n} lines from {}", path.display());
        if !is_remote_path(&path_str)
            && !is_compressed_path(&local)
            && !is_convertible_path(&local)
        {
            if let Ok(len) = file_len(&local) {
                offsets.insert(local.clone(), len);
            }
            format_hints.insert(local, hint);
        }
    }

    if follow {
        follow_files(
            &client,
            &url,
            &service,
            &mzp,
            &expanded,
            offsets,
            format_hints,
            test_follow_idle_limit(),
        )
        .await?;
    }
    Ok(())
}

fn test_follow_idle_limit() -> Option<usize> {
    if cfg!(test) {
        std::env::var("MIZPAH_TEST_FOLLOW_IDLE")
            .ok()
            .and_then(|v| v.parse().ok())
    } else {
        None
    }
}

#[allow(clippy::too_many_arguments)]
async fn follow_files(
    client: &reqwest::Client,
    url: &str,
    service: &str,
    mzp: &MzpMeta,
    paths: &[PathBuf],
    offsets: HashMap<PathBuf, u64>,
    format_hints: HashMap<PathBuf, Option<String>>,
    max_idle_timeouts: Option<usize>,
) -> Result<(), IngestError> {
    let (tx, rx) = mpsc::channel();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })
    .map_err(|e| IngestError::Message(e.to_string()))?;

    for p in paths {
        watcher
            .watch(p, RecursiveMode::NonRecursive)
            .map_err(|e| IngestError::Message(e.to_string()))?;
    }
    follow_files_loop(
        client,
        url,
        service,
        mzp,
        paths,
        offsets,
        format_hints,
        rx,
        max_idle_timeouts,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn follow_files_loop(
    client: &reqwest::Client,
    url: &str,
    service: &str,
    mzp: &MzpMeta,
    paths: &[PathBuf],
    mut offsets: HashMap<PathBuf, u64>,
    mut format_hints: HashMap<PathBuf, Option<String>>,
    rx: mpsc::Receiver<notify::Result<notify::Event>>,
    max_idle_timeouts: Option<usize>,
) -> Result<(), IngestError> {
    for p in paths {
        offsets.entry(p.clone()).or_insert(0);
    }

    eprintln!("following {} file(s); Ctrl-C to stop", paths.len());
    let mut idle = 0usize;
    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Ok(event)) => {
                idle = 0;
                if matches!(
                    event.kind,
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Any
                ) {
                    for p in event.paths {
                        if is_compressed_path(&p) {
                            if let Ok(reader) = open_reader(&p) {
                                let _ = ingest_reader(client, url, service, mzp, reader).await;
                            }
                            continue;
                        }
                        let offset = offsets.get(&p).copied().unwrap_or(0);
                        let hint = format_hints.get(&p).and_then(|h| h.as_deref());
                        match ingest_from_offset(client, url, service, mzp, &p, offset, hint).await
                        {
                            Ok((n, new_off)) => {
                                offsets.insert(p.clone(), new_off);
                                format_hints.entry(p).or_insert(None);
                                if n > 0 {
                                    eprintln!("followed +{n} line(s)");
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    path = %p.display(),
                                    "follow ingest failed"
                                );
                            }
                        }
                    }
                }
            }
            Ok(Err(e)) => return Err(IngestError::Message(e.to_string())),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tokio::task::yield_now().await;
                idle += 1;
                if max_idle_timeouts.is_some_and(|m| idle >= m) {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn detects_remote() {
        assert!(is_remote_path("user@host:/var/log/app.log"));
        assert!(is_remote_path("host:/var/log/app.log"));
        assert!(!is_remote_path("/var/log/app.log"));
        assert!(!is_remote_path("C:\\logs\\a.log"));
    }

    #[test]
    fn glob_star() {
        assert!(glob_match("*.log", "app.log"));
        assert!(!glob_match("*.log", "app.txt"));
    }

    #[test]
    fn offset_read_skips_existing_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        {
            let mut f = File::create(&path).unwrap();
            writeln!(f, r#"{{"msg":"a"}}"#).unwrap();
            writeln!(f, r#"{{"msg":"b"}}"#).unwrap();
        }
        let len1 = file_len(&path).unwrap();
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f, r#"{{"msg":"c"}}"#).unwrap();
        }
        let mut file = File::open(&path).unwrap();
        file.seek(SeekFrom::Start(len1)).unwrap();
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(line.contains("\"c\""));
        assert_eq!(reader.read_line(&mut String::new()).unwrap(), 0);
    }

    #[test]
    fn open_reader_plain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.log");
        std::fs::write(&path, "line1\nline2\n").unwrap();
        let mut reader = open_reader(&path).unwrap();
        let mut out = String::new();
        reader.read_to_string(&mut out).unwrap();
        assert_eq!(out, "line1\nline2\n");
    }

    #[test]
    fn open_reader_gzip() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.log.gz");
        let file = File::create(&path).unwrap();
        let mut encoder = GzEncoder::new(file, Compression::default());
        encoder.write_all(b"compressed\n").unwrap();
        encoder.finish().unwrap();

        let mut reader = open_reader(&path).unwrap();
        let mut out = String::new();
        reader.read_to_string(&mut out).unwrap();
        assert_eq!(out, "compressed\n");
    }

    // libbz2 FFI is unsupported under Miri.
    #[cfg(not(miri))]
    #[test]
    fn open_reader_bzip2() {
        use bzip2::write::BzEncoder;
        use bzip2::Compression;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.log.bz2");
        let file = File::create(&path).unwrap();
        let mut encoder = BzEncoder::new(file, Compression::default());
        encoder.write_all(b"bzipped\n").unwrap();
        encoder.finish().unwrap();

        let mut reader = open_reader(&path).unwrap();
        let mut out = String::new();
        reader.read_to_string(&mut out).unwrap();
        assert_eq!(out, "bzipped\n");
    }

    #[test]
    fn is_compressed_path_detects() {
        assert!(is_compressed_path(Path::new("app.log.gz")));
        assert!(is_compressed_path(Path::new("app.log.GZ")));
        assert!(is_compressed_path(Path::new("app.log.bz2")));
        assert!(is_compressed_path(Path::new("app.log.BZ2")));
        assert!(!is_compressed_path(Path::new("app.log")));
        assert!(!is_compressed_path(Path::new("app.txt")));
    }

    #[test]
    fn glob_match_basic() {
        assert!(glob_match("*.log", "app.log"));
        assert!(glob_match("test*", "test123"));
        assert!(glob_match("*test*", "123test456"));
        assert!(!glob_match("*.log", "app.txt"));
    }

    #[test]
    fn glob_match_question() {
        assert!(glob_match("app?.log", "app1.log"));
        assert!(glob_match("?.log", "a.log"));
        assert!(!glob_match("?.log", "ab.log"));
    }

    #[test]
    fn glob_match_exact() {
        assert!(glob_match("app.log", "app.log"));
        assert!(!glob_match("app.log", "app.txt"));
    }

    #[test]
    fn expand_paths_literal() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("a.log");
        std::fs::write(&p1, "").unwrap();
        let result = expand_paths(&[p1.to_string_lossy().to_string()]).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], p1);
    }

    #[test]
    fn expand_paths_glob() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("app1.log");
        let p2 = dir.path().join("app2.log");
        let p3 = dir.path().join("app.txt");
        std::fs::write(&p1, "").unwrap();
        std::fs::write(&p2, "").unwrap();
        std::fs::write(&p3, "").unwrap();

        let pattern = dir.path().join("*.log").to_string_lossy().to_string();
        let result = expand_paths(&[pattern]).unwrap();
        assert_eq!(result.len(), 2);
        assert!(result.contains(&p1));
        assert!(result.contains(&p2));
        assert!(!result.contains(&p3));
    }

    #[test]
    fn expand_paths_remote() {
        let result = expand_paths(&["user@host:/var/log/app.log".into()]).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].to_string_lossy(), "user@host:/var/log/app.log");
    }

    #[test]
    fn expand_paths_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let pattern = dir.path().join("*.log").to_string_lossy().to_string();
        let err = expand_paths(&[pattern]).unwrap_err();
        assert!(err.to_string().contains("no files matched"));
    }

    #[test]
    fn expand_paths_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("a.log");
        std::fs::write(&p1, "").unwrap();
        let s = p1.to_string_lossy().to_string();
        let result = expand_paths(&[s.clone(), s]).unwrap();
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn secure_mode_env_true() {
        temp_env::with_var("MIZPAH_SECURE", Some("1"), || {
            assert!(secure_mode());
        });
        temp_env::with_var("MIZPAH_SECURE", Some("true"), || {
            assert!(secure_mode());
        });
        temp_env::with_var("MIZPAH_SECURE", Some("TRUE"), || {
            assert!(secure_mode());
        });
        temp_env::with_var("MIZPAH_SECURE", Some("yes"), || {
            assert!(secure_mode());
        });
    }

    #[test]
    fn secure_mode_env_false() {
        temp_env::with_var("MIZPAH_SECURE", Some("0"), || {
            assert!(!secure_mode());
        });
        temp_env::with_var("MIZPAH_SECURE", Some("false"), || {
            assert!(!secure_mode());
        });
        temp_env::with_var("MIZPAH_SECURE", Some("no"), || {
            assert!(!secure_mode());
        });
    }

    #[cfg(not(miri))]
    use crate::test_support::spawn_test_hub;

    #[cfg(not(miri))]
    #[tokio::test]
    async fn ingest_from_offset_new_lines() {
        let (url, _store) = spawn_test_hub().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        {
            let mut f = File::create(&path).unwrap();
            writeln!(f, r#"{{"msg":"a"}}"#).unwrap();
            writeln!(f, r#"{{"msg":"b"}}"#).unwrap();
        }
        let len = file_len(&path).unwrap();

        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f, r#"{{"msg":"c"}}"#).unwrap();
            writeln!(f, r#"{{"msg":"d"}}"#).unwrap();
        }

        let client = crate::ingest_forward::http_client().unwrap();
        let mzp = crate::mzp_meta::MzpMeta::capture();
        let (count, new_offset) = ingest_from_offset(
            &client,
            &format!("{url}/api/ingest/batch"),
            "test",
            &mzp,
            &path,
            len,
            None,
        )
        .await
        .unwrap();

        assert_eq!(count, 2);
        assert!(new_offset > len);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn ingest_from_offset_truncated_file() {
        let (url, _store) = spawn_test_hub().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        {
            let mut f = File::create(&path).unwrap();
            writeln!(f, r#"{{"msg":"a"}}"#).unwrap();
        }

        // Truncate / replace file (with trailing newline so the line is complete)
        std::fs::write(&path, "{\"msg\":\"new\"}\n").unwrap();

        let client = crate::ingest_forward::http_client().unwrap();
        let mzp = crate::mzp_meta::MzpMeta::capture();
        let (count, new_offset) = ingest_from_offset(
            &client,
            &format!("{url}/api/ingest/batch"),
            "test",
            &mzp,
            &path,
            1000,
            None,
        )
        .await
        .unwrap();

        // Should restart from 0 and read the new content
        assert!(count < 10);
        assert!(new_offset > 0);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn ingest_from_offset_incomplete_line() {
        let (url, _store) = spawn_test_hub().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        {
            let mut f = File::create(&path).unwrap();
            write!(f, r#"{{"msg":"incomplete"#).unwrap();
        }

        let client = crate::ingest_forward::http_client().unwrap();
        let mzp = crate::mzp_meta::MzpMeta::capture();
        let (count, _) = ingest_from_offset(
            &client,
            &format!("{url}/api/ingest/batch"),
            "test",
            &mzp,
            &path,
            0,
            None,
        )
        .await
        .unwrap();

        // Should not ingest incomplete line
        assert_eq!(count, 0);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn ingest_from_offset_empty_file() {
        let (url, _store) = spawn_test_hub().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.log");
        std::fs::write(&path, "").unwrap();

        let client = crate::ingest_forward::http_client().unwrap();
        let mzp = crate::mzp_meta::MzpMeta::capture();
        let (count, offset) = ingest_from_offset(
            &client,
            &format!("{url}/api/ingest/batch"),
            "test",
            &mzp,
            &path,
            0,
            None,
        )
        .await
        .unwrap();

        assert_eq!(count, 0);
        assert_eq!(offset, 0);
    }

    #[test]
    fn fetch_remote_secure_mode_blocks() {
        temp_env::with_var("MIZPAH_SECURE", Some("1"), || {
            let err = fetch_remote_via_ssh("user@host:/path").unwrap_err();
            assert!(err.to_string().contains("refused"));
        });
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn ingest_reader_with_format_hint() {
        let (url, store) = spawn_test_hub().await;
        let client = crate::ingest_forward::http_client().unwrap();
        let mzp = crate::mzp_meta::MzpMeta::capture();
        let data = r#"{"msg":"line1"}
{"msg":"line2"}
{"msg":"line3"}
"#;
        let reader = std::io::BufReader::new(data.as_bytes());
        let (count, hint) = ingest_reader(
            &client,
            &format!("{url}/api/ingest/batch"),
            "test",
            &mzp,
            reader,
        )
        .await
        .unwrap();
        assert_eq!(count, 3);
        let _ = hint;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let entries = store.snapshot_entries().await;
        assert_eq!(entries.len(), 3);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn ingest_reader_empty() {
        let (url, _store) = spawn_test_hub().await;
        let client = crate::ingest_forward::http_client().unwrap();
        let mzp = crate::mzp_meta::MzpMeta::capture();
        let reader = std::io::BufReader::new("".as_bytes());
        let (count, hint) = ingest_reader(
            &client,
            &format!("{url}/api/ingest/batch"),
            "test",
            &mzp,
            reader,
        )
        .await
        .unwrap();
        assert_eq!(count, 0);
        assert!(hint.is_none());
    }

    #[test]
    fn file_len_returns_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.log");
        std::fs::write(&path, "hello\n").unwrap();
        let len = file_len(&path).unwrap();
        assert_eq!(len, 6);
    }

    #[test]
    fn file_len_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.log");
        let result = file_len(&path);
        assert!(result.is_err());
    }

    #[test]
    fn fetch_remote_invalid_spec() {
        temp_env::with_var("MIZPAH_SECURE", Some("0"), || {
            let err = fetch_remote_via_ssh("invalid-spec").unwrap_err();
            assert!(err.to_string().contains("invalid remote path"));
        });
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn run_ingest_rejects_follow_on_remote() {
        let (url, _store) = crate::test_support::spawn_test_hub().await;
        let port: u16 = url.rsplit(':').next().unwrap().parse().unwrap();
        let result = run_ingest(
            vec!["user@host:/var/log/app.log".into()],
            "test".into(),
            true,
            "127.0.0.1",
            port,
        )
        .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("--follow") && err.contains("remote"));
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn run_ingest_rejects_follow_on_binary() {
        let (url, _store) = crate::test_support::spawn_test_hub().await;
        let port: u16 = url.rsplit(':').next().unwrap().parse().unwrap();
        let evtx = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/sample.evtx");
        let result = run_ingest(
            vec![evtx.to_string_lossy().into_owned()],
            "test".into(),
            true,
            "127.0.0.1",
            port,
        )
        .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("--follow") && err.contains("binary"),
            "{err}"
        );
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn ingest_reader_large_batch() {
        let (url, _store) = spawn_test_hub().await;
        let client = crate::ingest_forward::http_client().unwrap();
        let mzp = crate::mzp_meta::MzpMeta::capture();
        let mut data = String::new();
        for i in 0..300 {
            data.push_str(&format!("{{\"msg\":\"line{i}\"}}\n"));
        }
        let reader = std::io::BufReader::new(data.as_bytes());
        let (count, _hint) = ingest_reader(
            &client,
            &format!("{url}/api/ingest/batch"),
            "test",
            &mzp,
            reader,
        )
        .await
        .unwrap();
        assert_eq!(count, 300);
    }

    #[cfg(unix)]
    fn ok_output(stdout: &[u8]) -> std::process::Output {
        use std::os::unix::process::ExitStatusExt;
        std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        }
    }

    #[cfg(unix)]
    fn fail_output(stderr: &[u8]) -> std::process::Output {
        use std::os::unix::process::ExitStatusExt;
        std::process::Output {
            status: std::process::ExitStatus::from_raw(256),
            stdout: Vec::new(),
            stderr: stderr.to_vec(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn fetch_remote_ssh_cat_success() {
        let path = fetch_remote_via_ssh_commands(
            "host:/var/log/a.log",
            |_, _| Ok(ok_output(b"{\"msg\":1}\n")),
            |_, _| unreachable!("scp should not run"),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"msg\":1}\n");
    }

    #[cfg(unix)]
    #[test]
    fn fetch_remote_falls_back_to_scp() {
        let path = fetch_remote_via_ssh_commands(
            "host:/var/log/a.log",
            |_, _| Ok(fail_output(b"nope")),
            |_, dest| {
                std::fs::write(dest, b"scp-body\n").unwrap();
                Ok(ok_output(b""))
            },
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "scp-body\n");
    }

    #[cfg(unix)]
    #[test]
    fn fetch_remote_both_fail() {
        let err = fetch_remote_via_ssh_commands(
            "host:/x",
            |_, _| Ok(fail_output(b"ssh err")),
            |_, _| Ok(fail_output(b"scp err")),
        )
        .unwrap_err();
        assert!(err.to_string().contains("remote fetch failed"));
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn follow_files_loop_idle_exit_and_modify() {
        let (hub_url, store) = spawn_test_hub().await;
        let client = crate::ingest_forward::http_client().unwrap();
        let mzp = crate::mzp_meta::MzpMeta::capture();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("follow.log");
        std::fs::write(&path, b"{\"msg\":\"old\"}\n").unwrap();

        let (tx, rx) = mpsc::channel();
        // Append a line and queue notify event before entering the loop.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"msg\":\"new\"}\n")
            .unwrap();
        tx.send(Ok(notify::Event {
            kind: EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Any,
            )),
            paths: vec![path.clone()],
            attrs: Default::default(),
        }))
        .unwrap();
        drop(tx);

        let paths = vec![path.clone()];
        let mut offsets = HashMap::new();
        offsets.insert(path.clone(), 0);
        let hints = HashMap::new();
        let url = format!("{hub_url}/api/ingest/batch");
        follow_files_loop(
            &client,
            &url,
            "follow",
            &mzp,
            &paths,
            offsets,
            hints,
            rx,
            Some(2),
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        let entries = store.snapshot_entries().await;
        assert!(entries.iter().any(|e| e.service == "follow"));
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn run_ingest_plain_file_no_follow() {
        let (hub_url, store) = spawn_test_hub().await;
        let port: u16 = hub_url.rsplit(':').next().unwrap().parse().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.log");
        std::fs::write(&path, b"{\"msg\":\"ingested\"}\n").unwrap();
        run_ingest(
            vec![path.to_string_lossy().into()],
            "file-svc".into(),
            false,
            "127.0.0.1",
            port,
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let entries = store.snapshot_entries().await;
        assert!(entries.iter().any(|e| e.service == "file-svc"));
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn run_ingest_remote_uses_hook() {
        let (hub_url, store) = spawn_test_hub().await;
        let port: u16 = hub_url.rsplit(':').next().unwrap().parse().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let remote_file = dir.path().join("remote.log");
        std::fs::write(&remote_file, b"{\"msg\":\"remote\"}\n").unwrap();
        // Leak path into static hook via env is awkward; use test_remote_fetch hook.
        fn hook(spec: &str) -> Result<PathBuf, IngestError> {
            let path = std::env::var("MIZPAH_TEST_REMOTE_FILE").unwrap();
            assert!(spec.contains(':'));
            Ok(PathBuf::from(path))
        }
        let _guard = crate::test_support::env_lock();
        std::env::set_var("MIZPAH_TEST_REMOTE_FILE", remote_file.to_str().unwrap());
        std::env::set_var("MIZPAH_SECURE", "0");
        test_remote_fetch::set(Some(hook));
        let result = run_ingest(
            vec!["host:/remote.log".into()],
            "remote-svc".into(),
            false,
            "127.0.0.1",
            port,
        )
        .await;
        test_remote_fetch::set(None);
        std::env::remove_var("MIZPAH_TEST_REMOTE_FILE");
        result.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let entries = store.snapshot_entries().await;
        assert!(entries.iter().any(|e| e.service == "remote-svc"));
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn follow_files_watcher_idle_exit() {
        let (hub_url, _store) = spawn_test_hub().await;
        let client = crate::ingest_forward::http_client().unwrap();
        let mzp = crate::mzp_meta::MzpMeta::capture();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("watch.log");
        std::fs::write(&path, b"{\"msg\":\"seed\"}\n").unwrap();
        follow_files(
            &client,
            &format!("{hub_url}/api/ingest/batch"),
            "watch",
            &mzp,
            std::slice::from_ref(&path),
            HashMap::new(),
            HashMap::new(),
            Some(1),
        )
        .await
        .unwrap();
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn run_ingest_with_follow_idle_limit() {
        let (hub_url, store) = spawn_test_hub().await;
        let port: u16 = hub_url.rsplit(':').next().unwrap().parse().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("follow-run.log");
        std::fs::write(&path, b"{\"msg\":\"start\"}\n").unwrap();
        let _guard = crate::test_support::env_lock();
        std::env::set_var("MIZPAH_TEST_FOLLOW_IDLE", "1");
        run_ingest(
            vec![path.to_string_lossy().into()],
            "follow-run".into(),
            true,
            "127.0.0.1",
            port,
        )
        .await
        .unwrap();
        std::env::remove_var("MIZPAH_TEST_FOLLOW_IDLE");
        tokio::time::sleep(Duration::from_millis(50)).await;
        let entries = store.snapshot_entries().await;
        assert!(entries.iter().any(|e| e.service == "follow-run"));
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn follow_files_loop_notify_error() {
        let (hub_url, _store) = spawn_test_hub().await;
        let client = crate::ingest_forward::http_client().unwrap();
        let mzp = crate::mzp_meta::MzpMeta::capture();
        let (tx, rx) = mpsc::channel();
        tx.send(Err(notify::Error::generic("watch failed")))
            .unwrap();
        drop(tx);
        let err = follow_files_loop(
            &client,
            &format!("{hub_url}/api/ingest/batch"),
            "svc",
            &mzp,
            &[],
            HashMap::new(),
            HashMap::new(),
            rx,
            None,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("watch failed"));
    }
}

// Helper for setting env vars in tests
#[cfg(test)]
mod temp_env {
    use std::env;

    pub fn with_var<F, T>(key: &str, value: Option<&str>, f: F) -> T
    where
        F: FnOnce() -> T,
    {
        let _guard = crate::test_support::env_lock();
        let old_value = env::var(key).ok();
        if let Some(v) = value {
            env::set_var(key, v);
        } else {
            env::remove_var(key);
        }
        let result = f();
        match old_value {
            Some(v) => env::set_var(key, v),
            None => env::remove_var(key),
        }
        result
    }
}
