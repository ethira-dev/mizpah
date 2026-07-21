//! Opt-in encrypted NDJSON segment persistence (Phase K).
//!
//! On-disk records are AES-256-GCM sealed lines (`mzp1:` + base64). Legacy
//! plaintext JSON lines are accepted on hydrate and rewritten encrypted.

use super::annotate::{AnnotatedEntry, Annotation};
use super::crypto::{
    looks_like_legacy_json_line, looks_like_sealed_line, load_log_crypto_at, LogCrypto,
};
use super::{LogEntry, Store};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

const SEGMENT_PREFIX: &str = "segment-";
const SEGMENT_SUFFIX: &str = ".ndjson";
const PERSIST_KIND_ANNOTATION: &str = "annotation";
/// Rotate to a new segment after this many bytes (approx).
#[cfg(not(test))]
const MAX_SEGMENT_BYTES: u64 = 64 * 1024 * 1024;
#[cfg(test)]
pub(crate) const MAX_SEGMENT_BYTES: u64 = 256;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistAnnotationLine {
    #[serde(rename = "_persistKind")]
    persist_kind: String,
    id: u64,
    annotation: Annotation,
}

/// Append-only encrypted persist writer held by the store when enabled.
pub struct PersistWriter {
    dir: PathBuf,
    file: Mutex<File>,
    segment_seq: AtomicU64,
    bytes_in_segment: AtomicU64,
    crypto: Arc<LogCrypto>,
}

impl PersistWriter {
    pub fn open(dir: &Path, crypto: Arc<LogCrypto>) -> std::io::Result<Self> {
        ensure_secure_persist_dir(dir)?;
        let seq = next_segment_seq(dir)?;
        let path = segment_path(dir, seq);
        let existing = path.metadata().map_or(0, |m| m.len());
        let file = open_segment_file(&path, existing > 0)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            file: Mutex::new(file),
            segment_seq: AtomicU64::new(seq),
            bytes_in_segment: AtomicU64::new(existing),
            crypto,
        })
    }

    async fn maybe_rotate(&self, upcoming_len: u64) -> std::io::Result<()> {
        let cur = self.bytes_in_segment.load(Ordering::Relaxed);
        if cur > 0 && cur.saturating_add(upcoming_len) > MAX_SEGMENT_BYTES {
            let next = self.segment_seq.load(Ordering::Relaxed).saturating_add(1);
            let path = segment_path(&self.dir, next);
            let file = open_segment_file(&path, false)?;
            let mut guard = self.file.lock().await;
            *guard = file;
            self.bytes_in_segment.store(0, Ordering::Relaxed);
            self.segment_seq.store(next, Ordering::Relaxed);
        }
        Ok(())
    }

    async fn append_sealed_line(&self, line: &str) -> std::io::Result<()> {
        let len = (line.len() + 1) as u64;
        self.maybe_rotate(len).await?;
        let mut file = self.file.lock().await;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        file.flush()?;
        self.bytes_in_segment.fetch_add(len, Ordering::Relaxed);
        Ok(())
    }

    pub async fn append_entry(&self, entry: &LogEntry) -> std::io::Result<()> {
        let plain = serde_json::to_vec(entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let line = self
            .crypto
            .seal_line(&plain)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        self.append_sealed_line(&line).await
    }

    pub async fn append_annotation(&self, id: u64, annotation: &Annotation) -> std::io::Result<()> {
        let line = PersistAnnotationLine {
            persist_kind: PERSIST_KIND_ANNOTATION.into(),
            id,
            annotation: annotation.clone(),
        };
        let plain = serde_json::to_vec(&line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let sealed = self
            .crypto
            .seal_line(&plain)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        self.append_sealed_line(&sealed).await
    }
}

fn ensure_secure_persist_dir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    refuse_symlink(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn refuse_symlink(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(std::io::Error::other(format!(
            "refusing symlink at {}",
            path.display()
        ))),
        Ok(_) | Err(_) => Ok(()),
    }
}

fn open_segment_file(path: &Path, append_existing: bool) -> std::io::Result<File> {
    if path.exists() {
        refuse_symlink(path)?;
    }
    let mut opts = OpenOptions::new();
    opts.create(true).append(true).write(true);
    if !append_existing && !path.exists() {
        opts.create_new(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let file = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(file)
}

fn segment_path(dir: &Path, seq: u64) -> PathBuf {
    dir.join(format!("{SEGMENT_PREFIX}{seq:06}{SEGMENT_SUFFIX}"))
}

fn next_segment_seq(dir: &Path) -> std::io::Result<u64> {
    let mut max = 0u64;
    if dir.is_dir() {
        for ent in fs::read_dir(dir)? {
            let ent = ent?;
            let name = ent.file_name();
            let name = name.to_string_lossy();
            if let Some(rest) = name
                .strip_prefix(SEGMENT_PREFIX)
                .and_then(|s| s.strip_suffix(SEGMENT_SUFFIX))
            {
                if let Ok(n) = rest.parse::<u64>() {
                    max = max.max(n);
                }
            }
        }
    }
    Ok(max.max(1))
}

fn list_segment_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(SEGMENT_PREFIX) && n.ends_with(SEGMENT_SUFFIX))
        })
        .collect();
    files.sort();
    Ok(files)
}

struct PersistLoad {
    entries: Vec<LogEntry>,
    annotations: Vec<AnnotatedEntry>,
    saw_legacy: bool,
}

/// Load all segment files (oldest first), decrypting sealed lines.
fn load_persist_dir(dir: &Path, crypto: &LogCrypto) -> std::io::Result<PersistLoad> {
    let files = list_segment_files(dir)?;
    let mut entries = Vec::new();
    let mut annotations = Vec::new();
    let mut saw_legacy = false;
    for path in files {
        refuse_symlink(&path)?;
        let file = File::open(&path)?;
        for line in BufReader::new(file).lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let plain = if looks_like_sealed_line(trimmed) {
                match crypto.open_line(trimmed) {
                    Ok(Some(bytes)) => bytes,
                    Ok(None) => continue,
                    Err(e) => {
                        tracing::warn!(error = %e, path = %path.display(), "skip bad sealed persist line");
                        continue;
                    }
                }
            } else if looks_like_legacy_json_line(trimmed) {
                saw_legacy = true;
                zeroize::Zeroizing::new(trimmed.as_bytes().to_vec())
            } else {
                tracing::warn!(path = %path.display(), "skip unrecognized persist line");
                continue;
            };
            let value: serde_json::Value = match serde_json::from_slice(plain.as_ref()) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(), "skip bad persist line");
                    continue;
                }
            };
            if value.get("_persistKind").and_then(|v| v.as_str()) == Some(PERSIST_KIND_ANNOTATION) {
                match serde_json::from_value::<PersistAnnotationLine>(value) {
                    Ok(ann) => annotations.push(AnnotatedEntry {
                        id: ann.id,
                        annotation: ann.annotation,
                    }),
                    Err(e) => {
                        tracing::warn!(error = %e, path = %path.display(), "skip bad annotation");
                    }
                }
                continue;
            }
            match serde_json::from_value::<LogEntry>(value) {
                Ok(mut entry) => {
                    entry.approx_bytes = 0;
                    entries.push(entry);
                }
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(), "skip bad persist line");
                }
            }
        }
    }
    Ok(PersistLoad {
        entries,
        annotations,
        saw_legacy,
    })
}

/// Rewrite retained entries/annotations as encrypted segments; delete prior files.
fn rewrite_encrypted_segments(
    dir: &Path,
    crypto: &LogCrypto,
    entries: &[LogEntry],
    annotations: &[AnnotatedEntry],
) -> std::io::Result<()> {
    ensure_secure_persist_dir(dir)?;
    let old = list_segment_files(dir)?;
    let tmp_dir = dir.join(format!(".rewrite-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp_dir);
    fs::create_dir_all(&tmp_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp_dir, fs::Permissions::from_mode(0o700));
    }

    let mut seq = 1u64;
    let mut bytes_in_seg = 0u64;
    let mut file = open_segment_file(&segment_path(&tmp_dir, seq), false)?;

    let mut write_line = |plain: &[u8]| -> std::io::Result<()> {
        let line = crypto
            .seal_line(plain)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let len = (line.len() + 1) as u64;
        if bytes_in_seg > 0 && bytes_in_seg.saturating_add(len) > MAX_SEGMENT_BYTES {
            seq += 1;
            file = open_segment_file(&segment_path(&tmp_dir, seq), false)?;
            bytes_in_seg = 0;
        }
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        bytes_in_seg += len;
        Ok(())
    };

    for entry in entries {
        let plain = serde_json::to_vec(entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        write_line(&plain)?;
    }
    for ann in annotations {
        let line = PersistAnnotationLine {
            persist_kind: PERSIST_KIND_ANNOTATION.into(),
            id: ann.id,
            annotation: ann.annotation.clone(),
        };
        let plain = serde_json::to_vec(&line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        write_line(&plain)?;
    }
    file.flush()?;

    for path in old {
        let _ = fs::remove_file(path);
    }
    for ent in fs::read_dir(&tmp_dir)? {
        let ent = ent?;
        let name = ent.file_name();
        fs::rename(ent.path(), dir.join(name))?;
    }
    let _ = fs::remove_dir_all(&tmp_dir);
    Ok(())
}

fn filter_entries_for_policy(
    mut entries: Vec<LogEntry>,
    ttl: Option<Duration>,
    max_bytes: u64,
) -> Vec<LogEntry> {
    entries.sort_by_key(|e| e.id);
    let mut approx: u64 = entries
        .iter()
        .map(|e| {
            if e.approx_bytes == 0 {
                super::ingest::estimate_bytes(&e.service, &e.data)
            } else {
                e.approx_bytes
            }
        })
        .sum();
    for e in &mut entries {
        if e.approx_bytes == 0 {
            e.approx_bytes = super::ingest::estimate_bytes(&e.service, &e.data);
        }
    }
    let mut start = 0usize;
    if let Some(ttl) = ttl {
        let now = Utc::now();
        while start < entries.len() {
            if !Store::entry_exceeds_ttl(entries[start].received_at, ttl, now) {
                break;
            }
            approx = approx.saturating_sub(entries[start].approx_bytes);
            start += 1;
        }
    }
    while approx > max_bytes && start < entries.len() {
        approx = approx.saturating_sub(entries[start].approx_bytes);
        start += 1;
    }
    entries.drain(start..).collect()
}

fn disk_crypto() -> std::io::Result<Arc<LogCrypto>> {
    let cfg = crate::util::config_dir()?;
    load_log_crypto_at(&cfg).map_err(|e| std::io::Error::other(e.to_string()))
}

impl Store {
    /// Enable append-only encrypted persistence under `dir`.
    pub async fn enable_persist(&self, dir: &Path) -> std::io::Result<()> {
        let crypto = disk_crypto()?;
        let writer = PersistWriter::open(dir, crypto)?;
        let mut guard = self.persist.write().await;
        *guard = Some(writer);
        Ok(())
    }

    /// Append a committed entry when persistence is enabled.
    pub(crate) async fn persist_entry(&self, entry: &LogEntry) {
        let guard = self.persist.read().await;
        if let Some(writer) = guard.as_ref() {
            if let Err(e) = writer.append_entry(entry).await {
                tracing::warn!(error = %e, "persist append failed");
            }
        }
    }

    /// Append an annotation record when persistence is enabled.
    pub(crate) async fn persist_annotation(&self, id: u64, annotation: &Annotation) {
        let guard = self.persist.read().await;
        if let Some(writer) = guard.as_ref() {
            if let Err(e) = writer.append_annotation(id, annotation).await {
                tracing::warn!(error = %e, "persist annotation failed");
            }
        }
    }

    /// Hydrate the ring buffer from encrypted (or legacy plaintext) segments.
    pub async fn hydrate_from_persist(&self, dir: &Path) -> std::io::Result<usize> {
        if !dir.is_dir() {
            return Ok(0);
        }
        let crypto = disk_crypto()?;

        let PersistLoad {
            entries,
            annotations,
            saw_legacy,
        } = load_persist_dir(dir, &crypto)?;
        if entries.is_empty() && annotations.is_empty() {
            return Ok(0);
        }

        let (max_bytes, ttl) = {
            let inner = self.inner.read().await;
            (inner.max_bytes, inner.ttl)
        };
        let mut entries = filter_entries_for_policy(entries, ttl, max_bytes);
        let live: std::collections::HashSet<u64> = entries.iter().map(|e| e.id).collect();
        let annotations: Vec<AnnotatedEntry> = annotations
            .into_iter()
            .filter(|a| live.contains(&a.id))
            .collect();

        // One-time migration / prune: rewrite disk to match retained encrypted set.
        if saw_legacy || needs_disk_prune(dir, &entries)? {
            if let Err(e) = rewrite_encrypted_segments(dir, &crypto, &entries, &annotations) {
                tracing::warn!(error = %e, "persist rewrite/migration failed");
            }
        }

        let mut max_id = 0u64;
        let n = entries.len();
        {
            let mut inner = self.inner.write().await;
            for mut entry in entries.drain(..) {
                max_id = max_id.max(entry.id);
                if entry.approx_bytes == 0 {
                    entry.approx_bytes = super::ingest::estimate_bytes(&entry.service, &entry.data);
                }
                *inner.services.entry(entry.service.clone()).or_insert(0) += 1;
                crate::properties::discover_paths_into(
                    &entry.data,
                    "",
                    &mut inner.properties,
                    true,
                );
                let svc_map = inner
                    .properties_by_service
                    .entry(entry.service.clone())
                    .or_default();
                crate::properties::discover_paths_into(&entry.data, "", svc_map, true);
                inner.approx_bytes += entry.approx_bytes;
                inner.entries.push_back(entry);
            }
            for ann in annotations {
                inner.annotations.insert(ann.id, ann.annotation);
            }
            let now = chrono::Utc::now();
            let _ = Self::evict_expired(&mut inner, now);
            let _ = Self::evict_over_capacity(&mut inner);
        }
        let next = max_id.saturating_add(1).max(1);
        self.next_id.store(next, Ordering::Relaxed);
        Ok(n)
    }

    /// Prune on-disk segments to the current in-memory ring (TTL / maxBytes).
    pub async fn prune_persist(&self) {
        let (dir, crypto) = {
            let guard = self.persist.read().await;
            match guard.as_ref() {
                Some(w) => (w.dir.clone(), Arc::clone(&w.crypto)),
                None => return,
            }
        };
        let (entries, annotations) = {
            let inner = self.inner.read().await;
            let entries: Vec<LogEntry> = inner.entries.iter().cloned().collect();
            let annotations: Vec<AnnotatedEntry> = inner
                .annotations
                .iter()
                .map(|(&id, annotation)| AnnotatedEntry {
                    id,
                    annotation: annotation.clone(),
                })
                .collect();
            (entries, annotations)
        };
        if let Err(e) = rewrite_encrypted_segments(&dir, &crypto, &entries, &annotations) {
            tracing::warn!(error = %e, "persist prune failed");
        } else {
            // Re-open writer at the highest segment after rewrite.
            match PersistWriter::open(&dir, crypto) {
                Ok(writer) => {
                    let mut guard = self.persist.write().await;
                    *guard = Some(writer);
                }
                Err(e) => tracing::warn!(error = %e, "persist re-open after prune failed"),
            }
        }
    }
}

fn needs_disk_prune(dir: &Path, retained: &[LogEntry]) -> std::io::Result<bool> {
    let mut total = 0u64;
    for path in list_segment_files(dir)? {
        total = total.saturating_add(path.metadata().map(|m| m.len()).unwrap_or(0));
    }
    let retained_bytes: u64 = retained.iter().map(|e| e.approx_bytes.max(1)).sum();
    // Rewrite when disk is much larger than retained payload (stale segments).
    Ok(total > retained_bytes.saturating_mul(4).max(4096))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::env_lock;
    use std::fs;

    struct ConfigEnv {
        _lock: crate::test_support::EnvLock,
        old_cfg: Option<std::ffi::OsString>,
        old_file: Option<std::ffi::OsString>,
        _tmp: tempfile::TempDir,
    }

    impl Drop for ConfigEnv {
        fn drop(&mut self) {
            match &self.old_cfg {
                Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
                None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
            }
            match &self.old_file {
                Some(v) => std::env::set_var("MIZPAH_USE_FILE_DEK", v),
                None => std::env::remove_var("MIZPAH_USE_FILE_DEK"),
            }
        }
    }

    fn with_config_dir() -> ConfigEnv {
        let tmp = tempfile::tempdir().unwrap();
        let lock = env_lock();
        let old_cfg = std::env::var_os("MIZPAH_CONFIG_DIR");
        let old_file = std::env::var_os("MIZPAH_USE_FILE_DEK");
        std::env::set_var("MIZPAH_CONFIG_DIR", tmp.path());
        std::env::set_var("MIZPAH_USE_FILE_DEK", "1");
        ConfigEnv {
            _lock: lock,
            old_cfg,
            old_file,
            _tmp: tmp,
        }
    }

    #[tokio::test]
    async fn persist_and_hydrate_roundtrip() {
        let _env = with_config_dir();
        let persist = tempfile::tempdir().unwrap();
        let store = Store::new(1_000_000);
        store.enable_persist(persist.path()).await.unwrap();
        store
            .push_line("api", r#"{"level":"info","msg":"persisted"}"#)
            .await;
        // Ciphertext should not contain plaintext msg.
        let raw = fs::read_to_string(persist.path().join("segment-000001.ndjson")).unwrap();
        assert!(!raw.contains("persisted"));
        assert!(raw.contains("mzp1:"));

        let store2 = Store::new(1_000_000);
        let n = store2.hydrate_from_persist(persist.path()).await.unwrap();
        assert_eq!(n, 1);
        assert_eq!(store2.stats().await.count, 1);
    }

    #[tokio::test]
    async fn persist_restores_annotations() {
        let _env = with_config_dir();
        let persist = tempfile::tempdir().unwrap();
        let store = Store::new(1_000_000);
        store.enable_persist(persist.path()).await.unwrap();
        let e = store
            .push_line("api", r#"{"level":"error","msg":"boom"}"#)
            .await;
        let id = e[0].id;
        store
            .set_bookmark(
                id,
                Some(true),
                Some(vec!["keep".into()]),
                Some(Some("note".into())),
            )
            .await
            .unwrap();

        let store2 = Store::new(1_000_000);
        let n = store2.hydrate_from_persist(persist.path()).await.unwrap();
        assert_eq!(n, 1);
        let ann = store2
            .get_annotation(id)
            .await
            .expect("annotation restored");
        assert!(ann.marked);
        assert_eq!(ann.tags, vec!["keep".to_string()]);
        assert_eq!(ann.comment.as_deref(), Some("note"));
    }

    #[tokio::test]
    async fn segment_rotates_when_byte_limit_exceeded() {
        let _env = with_config_dir();
        let persist = tempfile::tempdir().unwrap();
        let store = Store::new(1_000_000);
        store.enable_persist(persist.path()).await.unwrap();
        for i in 0..20 {
            store
                .push_line(
                    "api",
                    &format!(r#"{{"level":"info","msg":"{i}","pad":"xxxxxxxxxx"}}"#),
                )
                .await;
        }
        assert!(
            persist.path().join("segment-000001.ndjson").exists()
                || persist.path().join("segment-000002.ndjson").exists(),
            "expected rotation past {} bytes",
            super::MAX_SEGMENT_BYTES
        );
        let store2 = Store::new(1_000_000);
        let n = store2.hydrate_from_persist(persist.path()).await.unwrap();
        assert_eq!(n, 20);
    }

    #[tokio::test]
    async fn hydrate_migrates_legacy_plaintext() {
        let _env = with_config_dir();
        let persist = tempfile::tempdir().unwrap();
        let now = Utc::now().to_rfc3339();
        fs::write(
            persist.path().join("segment-000001.ndjson"),
            format!(
                "{{\"id\":1,\"receivedAt\":\"{now}\",\"service\":\"api\",\"data\":{{\"msg\":\"legacy-secret\"}}}}\n"
            ),
        )
        .unwrap();
        let store = Store::with_ttl_hours(1_000_000, 0);
        let n = store.hydrate_from_persist(persist.path()).await.unwrap();
        assert_eq!(n, 1);
        let raw = fs::read_to_string(persist.path().join("segment-000001.ndjson")).unwrap();
        assert!(!raw.contains("legacy-secret"));
        assert!(raw.contains("mzp1:"));
    }

    #[tokio::test]
    async fn hydrate_skips_bad_lines_and_orphan_annotations() {
        let _env = with_config_dir();
        let persist = tempfile::tempdir().unwrap();
        let now = Utc::now().to_rfc3339();
        // Legacy plaintext with a bad line — still migrates the good record.
        fs::write(
            persist.path().join("segment-000001.ndjson"),
            format!(
                "{{not json}}\n{{\"id\":1,\"receivedAt\":\"{now}\",\"service\":\"api\",\"data\":{{\"msg\":\"ok\"}}}}\n{{\"_persistKind\":\"annotation\",\"id\":999,\"annotation\":{{\"marked\":true,\"tags\":[]}}}}\n"
            ),
        )
        .unwrap();
        let store = Store::with_ttl_hours(1_000_000, 0);
        let n = store.hydrate_from_persist(persist.path()).await.unwrap();
        assert_eq!(n, 1);
        assert!(store.get_annotation(999).await.is_none());
    }

    #[tokio::test]
    async fn hydrate_empty_dir_is_noop() {
        let _env = with_config_dir();
        let persist = tempfile::tempdir().unwrap();
        let store = Store::new(1_000_000);
        assert_eq!(store.hydrate_from_persist(persist.path()).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn hydrate_non_dir_path_is_noop() {
        let _env = with_config_dir();
        let persist = tempfile::tempdir().unwrap();
        let file = persist.path().join("not-a-dir");
        fs::write(&file, b"x").unwrap();
        let store = Store::new(1_000_000);
        assert_eq!(store.hydrate_from_persist(&file).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn hydrate_skips_empty_lines_and_malformed_records() {
        let _env = with_config_dir();
        let persist = tempfile::tempdir().unwrap();
        let now = Utc::now().to_rfc3339();
        fs::write(
            persist.path().join("segment-000001.ndjson"),
            format!(
                "\n{{\"_persistKind\":\"annotation\",\"id\":1}}\n{{\"id\":2,\"receivedAt\":\"{now}\",\"service\":\"api\",\"data\":{{\"msg\":\"ok\"}}}}\n"
            ),
        )
        .unwrap();
        let store = Store::with_ttl_hours(1_000_000, 0);
        let n = store.hydrate_from_persist(persist.path()).await.unwrap();
        assert_eq!(n, 1);
        assert!(store.get_annotation(1).await.is_none());
    }

    #[tokio::test]
    async fn enable_persist_reuses_highest_segment_seq() {
        let _env = with_config_dir();
        let persist = tempfile::tempdir().unwrap();
        fs::write(persist.path().join("segment-000002.ndjson"), b"\n").unwrap();
        fs::write(persist.path().join("segment-000005.ndjson"), b"\n").unwrap();
        fs::write(persist.path().join("segment-bad-name.ndjson"), b"x").unwrap();
        let store = Store::new(1_000_000);
        store.enable_persist(persist.path()).await.unwrap();
        store.push_line("api", r#"{"msg":"after-open"}"#).await;
        assert!(
            persist
                .path()
                .join("segment-000005.ndjson")
                .metadata()
                .unwrap()
                .len()
                > 1
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn persist_files_are_private_mode() {
        use std::os::unix::fs::PermissionsExt;
        let _env = with_config_dir();
        let persist = tempfile::tempdir().unwrap();
        let store = Store::new(1_000_000);
        store.enable_persist(persist.path()).await.unwrap();
        store.push_line("api", r#"{"msg":"mode"}"#).await;
        let mode = fs::metadata(persist.path().join("segment-000001.ndjson"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
