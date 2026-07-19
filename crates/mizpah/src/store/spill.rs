//! Secure on-disk spill of the log buffer across self-update restarts.

use super::annotate::{AnnotatedEntry, Annotation};
use super::ingest::estimate_bytes;
use super::Store;
use crate::models::LogEntry;
use crate::properties::{rebuild_properties_by_service, rebuild_properties_from_entries};
use crate::util;
use chrono::Utc;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
type HmacSha256 = Hmac<Sha256>;

const SPILL_KIND_ANNOTATION: &str = "annotation";

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpillAnnotationLine {
    #[serde(rename = "_spillKind")]
    spill_kind: String,
    id: u64,
    annotation: Annotation,
}

const SPILL_BODY: &str = "update-spill.ndjson";
const SPILL_KEY: &str = "update-spill.key";
const SPILL_HMAC: &str = "update-spill.hmac";
/// Per-line cap to limit attacker-controlled JSON bombs during restore.
const MAX_LINE_BYTES: u64 = 16 * 1024 * 1024;
/// Slack above `max_bytes` for NDJSON framing overhead.
const SIZE_SLACK: u64 = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum SpillError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Msg(String),
}

impl SpillError {
    fn msg(s: impl Into<String>) -> Self {
        Self::Msg(s.into())
    }
}

fn spill_paths(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    (
        dir.join(SPILL_BODY),
        dir.join(SPILL_KEY),
        dir.join(SPILL_HMAC),
    )
}

/// Remove spill artifacts if present. Best-effort.
pub fn cleanup_spill_artifacts(dir: &Path) {
    let (body, key, mac) = spill_paths(dir);
    for path in [body, key, mac] {
        let _ = fs::remove_file(path);
    }
}

fn ensure_secure_config_dir(dir: &Path) -> Result<(), SpillError> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn refuse_symlink(path: &Path) -> Result<(), SpillError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(SpillError::msg(format!(
            "refusing symlink at {}",
            path.display()
        ))),
        Ok(_) | Err(_) => Ok(()),
    }
}

fn open_new_private_file(path: &Path) -> Result<File, SpillError> {
    if path.exists() {
        refuse_symlink(path)?;
        let _ = fs::remove_file(path);
    }
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
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

fn open_existing_private_file(path: &Path) -> Result<File, SpillError> {
    refuse_symlink(path)?;
    let meta = fs::metadata(path)?;
    if !meta.is_file() {
        return Err(SpillError::msg(format!(
            "spill path is not a regular file: {}",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        Ok(File::open(path)?)
    }
}

fn private_temp_path(final_path: &Path) -> PathBuf {
    let name = final_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("spill");
    final_path.with_file_name(format!(".{name}.tmp.{}", std::process::id()))
}

fn write_private_bytes(path: &Path, bytes: &[u8]) -> Result<(), SpillError> {
    let tmp = private_temp_path(path);
    {
        let mut f = open_new_private_file(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    if path.exists() {
        refuse_symlink(path)?;
    }
    fs::rename(&tmp, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

fn hex_decode(s: &str) -> Result<Vec<u8>, SpillError> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return Err(SpillError::msg("invalid hex length"));
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let nibble = |c: u8| -> Result<u8, SpillError> {
        match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            b'A'..=b'F' => Ok(c - b'A' + 10),
            _ => Err(SpillError::msg("invalid hex digit")),
        }
    };
    for chunk in bytes.chunks(2) {
        out.push((nibble(chunk[0])? << 4) | nibble(chunk[1])?);
    }
    Ok(out)
}

fn random_key() -> Result<[u8; 32], SpillError> {
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| SpillError::msg(format!("getrandom failed: {e}")))?;
    Ok(key)
}

impl Store {
    /// Spill the current buffer for a self-update restart (HMAC-protected).
    pub async fn spill_for_update(&self) -> Result<(), SpillError> {
        let dir = util::config_dir()?;
        self.spill_for_update_to(&dir).await
    }

    /// Restore a verified update spill if present. Always deletes artifacts afterward.
    pub async fn restore_update_spill(&self) -> Result<usize, SpillError> {
        let dir = match util::config_dir() {
            Ok(d) => d,
            Err(_) => return Ok(0),
        };
        self.restore_update_spill_from_dir(&dir).await
    }

    pub(crate) async fn spill_for_update_to(&self, dir: &Path) -> Result<(), SpillError> {
        ensure_secure_config_dir(dir)?;
        let (body_path, key_path, hmac_path) = spill_paths(dir);

        // Clear any previous artifacts first.
        cleanup_spill_artifacts(dir);

        let (entries, annotations): (Vec<LogEntry>, Vec<AnnotatedEntry>) = {
            let inner = self.inner.read().await;
            let entries = inner.entries.iter().cloned().collect();
            let annotations = inner
                .annotations
                .iter()
                .map(|(&id, annotation)| AnnotatedEntry {
                    id,
                    annotation: annotation.clone(),
                })
                .collect();
            (entries, annotations)
        };

        let tmp_body = private_temp_path(&body_path);
        let key = random_key()?;
        let mut mac =
            HmacSha256::new_from_slice(&key).map_err(|e| SpillError::msg(e.to_string()))?;

        {
            let mut file = open_new_private_file(&tmp_body)?;
            for entry in &entries {
                let line = serde_json::to_vec(entry)
                    .map_err(|e| SpillError::msg(format!("serialize spill entry: {e}")))?;
                file.write_all(&line)?;
                file.write_all(b"\n")?;
                mac.update(&line);
                mac.update(b"\n");
            }
            for ann in &annotations {
                let line = SpillAnnotationLine {
                    spill_kind: SPILL_KIND_ANNOTATION.into(),
                    id: ann.id,
                    annotation: ann.annotation.clone(),
                };
                let bytes = serde_json::to_vec(&line)
                    .map_err(|e| SpillError::msg(format!("serialize spill annotation: {e}")))?;
                file.write_all(&bytes)?;
                file.write_all(b"\n")?;
                mac.update(&bytes);
                mac.update(b"\n");
            }
            file.sync_all()?;
        }

        let digest = mac.finalize().into_bytes();
        write_private_bytes(&key_path, hex_encode(&key).as_bytes())?;
        write_private_bytes(&hmac_path, hex_encode(&digest).as_bytes())?;

        if body_path.exists() {
            refuse_symlink(&body_path)?;
        }
        fs::rename(&tmp_body, &body_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&body_path, fs::Permissions::from_mode(0o600));
        }

        Ok(())
    }

    pub(crate) async fn restore_update_spill_from_dir(
        &self,
        dir: &Path,
    ) -> Result<usize, SpillError> {
        let (body_path, key_path, hmac_path) = spill_paths(dir);
        if !body_path.exists() && !key_path.exists() && !hmac_path.exists() {
            return Ok(0);
        }

        let result = self.restore_update_spill_inner(dir).await;
        cleanup_spill_artifacts(dir);
        result
    }

    async fn restore_update_spill_inner(&self, dir: &Path) -> Result<usize, SpillError> {
        let (body_path, key_path, hmac_path) = spill_paths(dir);
        for path in [&body_path, &key_path, &hmac_path] {
            if !path.exists() {
                return Err(SpillError::msg(format!(
                    "incomplete spill package (missing {})",
                    path.display()
                )));
            }
            refuse_symlink(path)?;
        }

        let max_bytes = {
            let inner = self.inner.read().await;
            inner.max_bytes
        };
        let max_file = max_bytes.saturating_add(SIZE_SLACK);
        let body_meta = fs::metadata(&body_path)?;
        if body_meta.len() > max_file {
            return Err(SpillError::msg(format!(
                "spill file too large ({} > {max_file})",
                body_meta.len()
            )));
        }

        let mut key_hex = String::new();
        open_existing_private_file(&key_path)?.read_to_string(&mut key_hex)?;
        let key = hex_decode(&key_hex)?;
        if key.len() != 32 {
            return Err(SpillError::msg("spill key must be 32 bytes"));
        }

        let mut expected_hex = String::new();
        open_existing_private_file(&hmac_path)?.read_to_string(&mut expected_hex)?;
        let expected = hex_decode(&expected_hex)?;

        let mut mac =
            HmacSha256::new_from_slice(&key).map_err(|e| SpillError::msg(e.to_string()))?;
        let mut entries = Vec::new();
        let mut annotations = Vec::new();
        {
            let file = open_existing_private_file(&body_path)?;
            let mut reader = BufReader::new(file);
            let mut line_buf = Vec::new();
            loop {
                line_buf.clear();
                let n = reader.read_until(b'\n', &mut line_buf)?;
                if n == 0 {
                    break;
                }
                mac.update(&line_buf);
                // Trim trailing newline for parse; keep original bytes in mac.
                let trimmed = if line_buf.last() == Some(&b'\n') {
                    &line_buf[..line_buf.len() - 1]
                } else {
                    line_buf.as_slice()
                };
                if trimmed.is_empty() {
                    continue;
                }
                if trimmed.len() as u64 > MAX_LINE_BYTES {
                    return Err(SpillError::msg("spill line exceeds size limit"));
                }
                let value: Value = serde_json::from_slice(trimmed)
                    .map_err(|e| SpillError::msg(format!("invalid spill line: {e}")))?;
                if value
                    .get("_spillKind")
                    .and_then(|v| v.as_str())
                    == Some(SPILL_KIND_ANNOTATION)
                {
                    let ann: SpillAnnotationLine = serde_json::from_value(value).map_err(|e| {
                        SpillError::msg(format!("invalid spill annotation: {e}"))
                    })?;
                    annotations.push(AnnotatedEntry {
                        id: ann.id,
                        annotation: ann.annotation,
                    });
                    continue;
                }
                let mut entry: LogEntry = serde_json::from_value(value)
                    .map_err(|e| SpillError::msg(format!("invalid spill entry: {e}")))?;
                entry.approx_bytes = estimate_bytes(&entry.service, &entry.data);
                entries.push(entry);
            }
        }

        mac.verify_slice(&expected)
            .map_err(|_| SpillError::msg("spill HMAC verification failed"))?;

        let count = self.load_spilled_entries(entries).await;
        self.load_spilled_annotations(annotations).await;
        Ok(count)
    }

    async fn load_spilled_annotations(&self, annotations: Vec<AnnotatedEntry>) {
        if annotations.is_empty() {
            return;
        }
        let mut inner = self.inner.write().await;
        let live: HashSet<u64> = inner.entries.iter().map(|e| e.id).collect();
        for ann in annotations {
            if !live.contains(&ann.id) {
                continue;
            }
            inner.annotations.insert(ann.id, ann.annotation);
        }
    }

    /// Bulk-load verified entries, preserving ids/timestamps and rebuilding indexes.
    async fn load_spilled_entries(&self, mut entries: Vec<LogEntry>) -> usize {
        if entries.is_empty() {
            return 0;
        }
        entries.sort_by_key(|e| e.id);

        let (max_bytes, ttl) = {
            let inner = self.inner.read().await;
            (inner.max_bytes, inner.ttl)
        };

        let mut approx_bytes: u64 = entries.iter().map(|e| e.approx_bytes).sum();
        let mut start = 0usize;
        if let Some(ttl) = ttl {
            let now = Utc::now();
            while start < entries.len() {
                if !Store::entry_exceeds_ttl(entries[start].received_at, ttl, now) {
                    break;
                }
                approx_bytes = approx_bytes.saturating_sub(entries[start].approx_bytes);
                start += 1;
            }
        }
        while approx_bytes > max_bytes && start < entries.len() {
            approx_bytes = approx_bytes.saturating_sub(entries[start].approx_bytes);
            start += 1;
        }
        let kept: VecDeque<LogEntry> = entries.drain(start..).collect();
        let count = kept.len();
        let next_id = kept
            .iter()
            .map(|e| e.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);

        let mut services: HashMap<String, u64> = HashMap::new();
        for entry in &kept {
            *services.entry(entry.service.clone()).or_insert(0) += 1;
        }
        let properties = rebuild_properties_from_entries(&kept);
        let properties_by_service = rebuild_properties_by_service(&kept);

        {
            let mut inner = self.inner.write().await;
            inner.entries = kept;
            inner.approx_bytes = approx_bytes;
            inner.services = services;
            inner.properties = properties;
            inner.properties_by_service = properties_by_service;
            // Keep any blocked set empty on fresh hub; pretty buffers stay empty.
        }
        self.next_id.store(next_id.max(1), Ordering::Relaxed);
        count
    }
}

/// Test helpers and unit tests for spill security.
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;
    use std::sync::atomic::Ordering;

    fn entry(id: u64, service: &str, msg: &str) -> LogEntry {
        let data = json!({"msg": msg, "level": "info"});
        let approx_bytes = estimate_bytes(service, &data);
        LogEntry {
            id,
            received_at: Utc::now(),
            event_time: None,
            service: service.into(),
            format_id: Some("json".into()),
            data,
            approx_bytes,
        }
    }

    #[tokio::test]
    async fn spill_roundtrip_preserves_entries() {
        let dir = tempfile::tempdir().unwrap();

        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"msg":"one","level":"info"}"#)
            .await;
        store
            .push_line("api", r#"{"msg":"two","level":"warn"}"#)
            .await;
        let before = {
            let inner = store.inner.read().await;
            inner.entries.clone()
        };
        let mark_id = before[0].id;
        store
            .set_bookmark(
                mark_id,
                Some(true),
                Some(vec!["keep".into()]),
                Some(Some("note".into())),
            )
            .await
            .unwrap();

        store.spill_for_update_to(dir.path()).await.expect("spill");

        let restored = Store::new(1_000_000);
        let n = restored
            .restore_update_spill_from_dir(dir.path())
            .await
            .expect("restore");
        assert_eq!(n, 2);
        let after = {
            let inner = restored.inner.read().await;
            inner.entries.clone()
        };
        let ann = restored.get_annotation(mark_id).await.expect("annotation");
        assert!(ann.marked);
        assert_eq!(ann.tags, vec!["keep".to_string()]);
        assert_eq!(ann.comment.as_deref(), Some("note"));
        assert_eq!(after.len(), before.len());
        for (a, b) in before.iter().zip(after.iter()) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.service, b.service);
            assert_eq!(a.data, b.data);
            assert_eq!(a.received_at, b.received_at);
        }
        assert_eq!(restored.next_id.load(Ordering::Relaxed), 3);
        assert!(!dir.path().join(SPILL_BODY).exists());
        assert!(!dir.path().join(SPILL_KEY).exists());
        assert!(!dir.path().join(SPILL_HMAC).exists());
    }

    #[tokio::test]
    async fn tampered_body_is_rejected() {
        let dir = tempfile::tempdir().unwrap();

        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"msg":"ok","level":"info"}"#)
            .await;
        store.spill_for_update_to(dir.path()).await.unwrap();

        let body = dir.path().join(SPILL_BODY);
        fs::write(&body, b"{\"id\":1,\"receivedAt\":\"2020-01-01T00:00:00Z\",\"service\":\"x\",\"data\":{\"msg\":\"pwned\"}}\n").unwrap();

        let restored = Store::new(1_000_000);
        let err = restored
            .restore_update_spill_from_dir(dir.path())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("HMAC"), "{err}");
        assert!(restored.inner.read().await.entries.is_empty());
        assert!(!body.exists());
    }

    #[tokio::test]
    async fn oversized_spill_is_rejected() {
        let dir = tempfile::tempdir().unwrap();

        let tiny = Store::new(100);
        {
            let mut inner = tiny.inner.write().await;
            inner.entries.push_back(entry(1, "api", "hi"));
            inner.approx_bytes = inner.entries[0].approx_bytes;
        }
        tiny.spill_for_update_to(dir.path()).await.unwrap();

        // Inflate body beyond tiny max + slack (size check runs before HMAC).
        let body = dir.path().join(SPILL_BODY);
        let padding = vec![b'x'; (SIZE_SLACK as usize) + 200];
        let mut data = fs::read(&body).unwrap();
        data.extend_from_slice(&padding);
        fs::write(&body, data).unwrap();

        let restored = Store::new(100);
        let err = restored
            .restore_update_spill_from_dir(dir.path())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("too large"), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_body_is_rejected() {
        let dir = tempfile::tempdir().unwrap();

        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"msg":"ok","level":"info"}"#)
            .await;
        store.spill_for_update_to(dir.path()).await.unwrap();

        let body = dir.path().join(SPILL_BODY);
        let target = dir.path().join("evil-target");
        fs::write(&target, b"nope\n").unwrap();
        fs::remove_file(&body).unwrap();
        std::os::unix::fs::symlink(&target, &body).unwrap();

        let restored = Store::new(1_000_000);
        let err = restored
            .restore_update_spill_from_dir(dir.path())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
        assert!(!dir.path().join(SPILL_KEY).exists());
    }

    #[tokio::test]
    async fn missing_spill_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(1_000_000);
        assert_eq!(
            store
                .restore_update_spill_from_dir(dir.path())
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn incomplete_spill_package_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"msg":"ok","level":"info"}"#)
            .await;
        store.spill_for_update_to(dir.path()).await.unwrap();
        fs::remove_file(dir.path().join(SPILL_KEY)).unwrap();

        let restored = Store::new(1_000_000);
        let err = restored
            .restore_update_spill_from_dir(dir.path())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("incomplete"), "{err}");
    }

    #[tokio::test]
    async fn invalid_spill_hex_and_key_length_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"msg":"ok","level":"info"}"#)
            .await;
        store.spill_for_update_to(dir.path()).await.unwrap();

        fs::write(dir.path().join(SPILL_KEY), b"zz\n").unwrap();
        let err = Store::new(1_000_000)
            .restore_update_spill_from_dir(dir.path())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("hex"), "{err}");

        store.spill_for_update_to(dir.path()).await.unwrap();
        fs::write(dir.path().join(SPILL_KEY), b"0102\n").unwrap();
        let err = Store::new(1_000_000)
            .restore_update_spill_from_dir(dir.path())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("32 bytes"), "{err}");
    }

    #[tokio::test]
    async fn orphan_annotations_are_ignored_on_restore() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(1_000_000);
        let entries = store
            .push_line("api", r#"{"msg":"only","level":"info"}"#)
            .await;
        let live_id = entries[0].id;
        store
            .set_bookmark(live_id, Some(true), None, None)
            .await
            .unwrap();
        store.spill_for_update_to(dir.path()).await.unwrap();

        let body_path = dir.path().join(SPILL_BODY);
        let key_path = dir.path().join(SPILL_KEY);
        let hmac_path = dir.path().join(SPILL_HMAC);
        let key_hex = fs::read_to_string(&key_path).unwrap();
        let key = hex_decode(&key_hex).unwrap();
        let mut body = fs::read(&body_path).unwrap();
        let orphan = serde_json::to_vec(&SpillAnnotationLine {
            spill_kind: SPILL_KIND_ANNOTATION.into(),
            id: live_id + 10_000,
            annotation: Annotation {
                marked: true,
                tags: vec!["orphan".into()],
                comment: None,
            },
        })
        .unwrap();
        body.extend_from_slice(&orphan);
        body.push(b'\n');
        fs::write(&body_path, &body).unwrap();
        let mut mac =
            HmacSha256::new_from_slice(&key).expect("key");
        mac.update(&body);
        fs::write(&hmac_path, hex_encode(&mac.finalize().into_bytes())).unwrap();

        let restored = Store::new(1_000_000);
        let n = restored
            .restore_update_spill_from_dir(dir.path())
            .await
            .expect("restore");
        assert_eq!(n, 1);
        assert!(restored.get_annotation(live_id).await.unwrap().marked);
        assert!(restored.get_annotation(live_id + 10_000).await.is_none());
    }

    #[test]
    fn hex_decode_rejects_odd_length_and_bad_digits() {
        assert!(hex_decode("abc").is_err());
        assert!(hex_decode("gg").is_err());
    }

    #[tokio::test]
    async fn invalid_spill_line_json_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"msg":"ok","level":"info"}"#)
            .await;
        store.spill_for_update_to(dir.path()).await.unwrap();

        let body_path = dir.path().join(SPILL_BODY);
        let key_path = dir.path().join(SPILL_KEY);
        let hmac_path = dir.path().join(SPILL_HMAC);
        let key = hex_decode(&fs::read_to_string(&key_path).unwrap()).unwrap();
        let mut body = b"not-json\n".to_vec();
        body.extend_from_slice(&fs::read(&body_path).unwrap());
        fs::write(&body_path, &body).unwrap();
        let mut mac = HmacSha256::new_from_slice(&key).unwrap();
        mac.update(&body);
        fs::write(&hmac_path, hex_encode(&mac.finalize().into_bytes())).unwrap();

        let err = Store::new(1_000_000)
            .restore_update_spill_from_dir(dir.path())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid spill line"), "{err}");
    }

    #[test]
    fn hex_decode_accepts_uppercase() {
        assert_eq!(hex_decode("ABCD").unwrap(), vec![0xab, 0xcd]);
    }

    #[tokio::test]
    async fn cleanup_spill_artifacts_removes_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"msg":"ok","level":"info"}"#)
            .await;
        store.spill_for_update_to(dir.path()).await.unwrap();
        cleanup_spill_artifacts(dir.path());
        assert!(!dir.path().join(SPILL_BODY).exists());
        assert!(!dir.path().join(SPILL_KEY).exists());
        assert!(!dir.path().join(SPILL_HMAC).exists());
    }

    #[tokio::test]
    async fn restore_update_spill_missing_config_dir_is_noop() {
        let _guard = crate::test_support::env_lock();
        let old = std::env::var_os("MIZPAH_CONFIG_DIR");
        std::env::set_var("MIZPAH_CONFIG_DIR", "/no/such/mizpah-config-dir");
        let store = Store::new(1_000_000);
        assert_eq!(store.restore_update_spill().await.unwrap(), 0);
        match old {
            Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
            None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
        }
    }

    #[tokio::test]
    async fn oversized_spill_line_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"msg":"ok","level":"info"}"#)
            .await;
        store.spill_for_update_to(dir.path()).await.unwrap();

        let body_path = dir.path().join(SPILL_BODY);
        let key_path = dir.path().join(SPILL_KEY);
        let hmac_path = dir.path().join(SPILL_HMAC);
        let key = hex_decode(&fs::read_to_string(&key_path).unwrap()).unwrap();
        let mut body = fs::read(&body_path).unwrap();
        let huge = vec![b'x'; (MAX_LINE_BYTES as usize) + 1];
        body.extend_from_slice(&huge);
        body.push(b'\n');
        fs::write(&body_path, &body).unwrap();
        let mut mac = HmacSha256::new_from_slice(&key).unwrap();
        mac.update(&body);
        fs::write(&hmac_path, hex_encode(&mac.finalize().into_bytes())).unwrap();

        let err = Store::new(1_000_000)
            .restore_update_spill_from_dir(dir.path())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("too large") || err.to_string().contains("size limit"), "{err}");
    }

    #[tokio::test]
    async fn invalid_spill_entry_json_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"msg":"ok","level":"info"}"#)
            .await;
        store.spill_for_update_to(dir.path()).await.unwrap();

        let body_path = dir.path().join(SPILL_BODY);
        let key_path = dir.path().join(SPILL_KEY);
        let hmac_path = dir.path().join(SPILL_HMAC);
        let key = hex_decode(&fs::read_to_string(&key_path).unwrap()).unwrap();
        let bad = b"{\"not\":\"a log entry\"}\n".to_vec();
        fs::write(&body_path, &bad).unwrap();
        let mut mac = HmacSha256::new_from_slice(&key).unwrap();
        mac.update(&bad);
        fs::write(&hmac_path, hex_encode(&mac.finalize().into_bytes())).unwrap();

        let err = Store::new(1_000_000)
            .restore_update_spill_from_dir(dir.path())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid spill entry"), "{err}");
    }

    #[tokio::test]
    async fn restore_trims_entries_by_ttl_and_max_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(200);
        store.inner.write().await.ttl = Some(std::time::Duration::from_secs(3600));
        let old = chrono::Utc::now() - chrono::Duration::hours(2);
        {
            let mut inner = store.inner.write().await;
            for i in 1..=3 {
                let data = json!({"msg": format!("m{i}"), "level": "info"});
                inner.entries.push_back(LogEntry {
                    id: i,
                    received_at: if i == 1 { old } else { chrono::Utc::now() },
                    event_time: None,
                    service: "api".into(),
                    format_id: Some("json".into()),
                    data: data.clone(),
                    approx_bytes: estimate_bytes("api", &data),
                });
            }
            inner.approx_bytes = inner.entries.iter().map(|e| e.approx_bytes).sum();
        }
        store.spill_for_update_to(dir.path()).await.unwrap();
        let restored = Store::new(200);
        restored.inner.write().await.ttl = Some(std::time::Duration::from_secs(3600));
        let n = restored
            .restore_update_spill_from_dir(dir.path())
            .await
            .unwrap();
        assert_eq!(n, 2);
        let inner = restored.inner.read().await;
        assert_eq!(inner.entries.len(), 2);
        assert!(inner.entries.front().unwrap().id >= 2);
    }
}
