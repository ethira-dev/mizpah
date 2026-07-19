//! Opt-in NDJSON segment persistence (Phase K).

use super::annotate::{AnnotatedEntry, Annotation};
use super::{LogEntry, Store};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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

/// Append-only persist writer held by the store when enabled.
pub struct PersistWriter {
    dir: PathBuf,
    file: Mutex<File>,
    segment_seq: AtomicU64,
    bytes_in_segment: AtomicU64,
}

impl PersistWriter {
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        fs::create_dir_all(dir)?;
        let seq = next_segment_seq(dir)?;
        let path = segment_path(dir, seq);
        let existing = path.metadata().map_or(0, |m| m.len());
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            file: Mutex::new(file),
            segment_seq: AtomicU64::new(seq),
            bytes_in_segment: AtomicU64::new(existing),
        })
    }

    async fn maybe_rotate(&self, upcoming_len: u64) -> std::io::Result<()> {
        let cur = self.bytes_in_segment.load(Ordering::Relaxed);
        if cur > 0 && cur.saturating_add(upcoming_len) > MAX_SEGMENT_BYTES {
            let next = self.segment_seq.load(Ordering::Relaxed).saturating_add(1);
            let path = segment_path(&self.dir, next);
            let file = OpenOptions::new().create(true).append(true).open(&path)?;
            let mut guard = self.file.lock().await;
            *guard = file;
            self.bytes_in_segment.store(0, Ordering::Relaxed);
            self.segment_seq.store(next, Ordering::Relaxed);
        }
        Ok(())
    }

    pub async fn append_entry(&self, entry: &LogEntry) -> std::io::Result<()> {
        let line = serde_json::to_string(entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let len = (line.len() + 1) as u64;
        self.maybe_rotate(len).await?;
        let mut file = self.file.lock().await;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        file.flush()?;
        self.bytes_in_segment.fetch_add(len, Ordering::Relaxed);
        Ok(())
    }

    pub async fn append_annotation(&self, id: u64, annotation: &Annotation) -> std::io::Result<()> {
        let line = PersistAnnotationLine {
            persist_kind: PERSIST_KIND_ANNOTATION.into(),
            id,
            annotation: annotation.clone(),
        };
        let bytes = serde_json::to_vec(&line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let len = (bytes.len() + 1) as u64;
        self.maybe_rotate(len).await?;
        let mut file = self.file.lock().await;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.flush()?;
        self.bytes_in_segment.fetch_add(len, Ordering::Relaxed);
        Ok(())
    }
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

struct PersistLoad {
    entries: Vec<LogEntry>,
    annotations: Vec<AnnotatedEntry>,
}

/// Load all NDJSON segment files (oldest segment first).
fn load_persist_dir(dir: &Path) -> std::io::Result<PersistLoad> {
    if !dir.is_dir() {
        return Ok(PersistLoad {
            entries: Vec::new(),
            annotations: Vec::new(),
        });
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
    let mut entries = Vec::new();
    let mut annotations = Vec::new();
    for path in files {
        let file = File::open(&path)?;
        for line in BufReader::new(file).lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value: serde_json::Value = match serde_json::from_str(trimmed) {
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
    })
}

impl Store {
    /// Enable append-only NDJSON persistence under `dir`.
    pub async fn enable_persist(&self, dir: &Path) -> std::io::Result<()> {
        let writer = PersistWriter::open(dir)?;
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

    /// Hydrate the ring buffer from NDJSON segments (ids preserved; next_id advanced).
    pub async fn hydrate_from_persist(&self, dir: &Path) -> std::io::Result<usize> {
        let PersistLoad {
            entries,
            annotations,
        } = load_persist_dir(dir)?;
        if entries.is_empty() && annotations.is_empty() {
            return Ok(0);
        }
        let mut max_id = 0u64;
        let n = entries.len();
        {
            let mut inner = self.inner.write().await;
            for mut entry in entries {
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
            let live: std::collections::HashSet<u64> = inner.entries.iter().map(|e| e.id).collect();
            for ann in annotations {
                if live.contains(&ann.id) {
                    inner.annotations.insert(ann.id, ann.annotation);
                }
            }
            let now = chrono::Utc::now();
            let _ = Self::evict_expired(&mut inner, now);
            let _ = Self::evict_over_capacity(&mut inner);
        }
        let next = max_id.saturating_add(1).max(1);
        self.next_id.store(next, Ordering::Relaxed);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn persist_and_hydrate_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(1_000_000);
        store.enable_persist(dir.path()).await.unwrap();
        store
            .push_line("api", r#"{"level":"info","msg":"persisted"}"#)
            .await;
        let store2 = Store::new(1_000_000);
        let n = store2.hydrate_from_persist(dir.path()).await.unwrap();
        assert_eq!(n, 1);
        assert_eq!(store2.stats().await.count, 1);
    }

    #[tokio::test]
    async fn persist_restores_annotations() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(1_000_000);
        store.enable_persist(dir.path()).await.unwrap();
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
        let n = store2.hydrate_from_persist(dir.path()).await.unwrap();
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
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(1_000_000);
        store.enable_persist(dir.path()).await.unwrap();
        for i in 0..20 {
            store
                .push_line(
                    "api",
                    &format!(r#"{{"level":"info","msg":"{i}","pad":"xxxxxxxxxx"}}"#),
                )
                .await;
        }
        assert!(
            dir.path().join("segment-000001.ndjson").exists(),
            "expected rotation past {} bytes",
            super::MAX_SEGMENT_BYTES
        );
        let store2 = Store::new(1_000_000);
        let n = store2.hydrate_from_persist(dir.path()).await.unwrap();
        assert_eq!(n, 20);
    }

    #[tokio::test]
    async fn hydrate_skips_bad_lines_and_orphan_annotations() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("segment-000001.ndjson"),
            b"{not json}\n{\"id\":1,\"receivedAt\":\"2024-01-01T00:00:00Z\",\"service\":\"api\",\"data\":{\"msg\":\"ok\"}}\n{\"_persistKind\":\"annotation\",\"id\":999,\"annotation\":{\"marked\":true,\"tags\":[]}}\n",
        )
        .unwrap();
        let store = Store::new(1_000_000);
        let n = store.hydrate_from_persist(dir.path()).await.unwrap();
        assert_eq!(n, 1);
        assert!(store.get_annotation(999).await.is_none());
    }

    #[tokio::test]
    async fn hydrate_empty_dir_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(1_000_000);
        assert_eq!(store.hydrate_from_persist(dir.path()).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn hydrate_non_dir_path_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir");
        fs::write(&file, b"x").unwrap();
        let store = Store::new(1_000_000);
        assert_eq!(store.hydrate_from_persist(&file).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn hydrate_skips_empty_lines_and_malformed_records() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("segment-000001.ndjson"),
            b"\n{\"_persistKind\":\"annotation\",\"id\":1}\n{\"id\":2,\"receivedAt\":\"2024-01-01T00:00:00Z\",\"service\":\"api\",\"data\":{\"msg\":\"ok\"}}\n",
        )
        .unwrap();
        let store = Store::new(1_000_000);
        let n = store.hydrate_from_persist(dir.path()).await.unwrap();
        assert_eq!(n, 1);
        assert!(store.get_annotation(1).await.is_none());
    }

    #[tokio::test]
    async fn enable_persist_reuses_highest_segment_seq() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("segment-000002.ndjson"), b"\n").unwrap();
        fs::write(dir.path().join("segment-000005.ndjson"), b"\n").unwrap();
        fs::write(dir.path().join("segment-bad-name.ndjson"), b"x").unwrap();
        let store = Store::new(1_000_000);
        store.enable_persist(dir.path()).await.unwrap();
        store.push_line("api", r#"{"msg":"after-open"}"#).await;
        assert!(
            dir.path()
                .join("segment-000005.ndjson")
                .metadata()
                .unwrap()
                .len()
                > 1
        );
    }
}
