use crate::pretty_ingest::{
    is_pretty_block_start, joined_raw_payload, recover_json_object, strip_ansi,
    strip_service_prefix, PrettyBuffer,
};
use crate::properties::{
    decrement_counts_for_entry, discover_paths_into, paths_to_info, push_service_property,
    rebuild_properties_by_service, rebuild_properties_from_entries, PathMeta,
};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};

// Re-export public DTOs so existing `crate::store::{...}` imports keep working.
pub use crate::models::{ActivityBucket, LogEntry, PropertyInfo, ServiceInfo, Stats, WsEvent};
pub use crate::mzp_meta::MzpMeta;

mod activity;
mod aggregate;
mod annotate;
mod crypto;
mod ingest;
mod nav;
mod persist;
mod query;
mod redact;
mod spectrogram;
mod spill;
mod trace;

pub use aggregate::{AggregateMetrics, AggregateRow};
pub use ingest::parse_line;
pub use nav::NavDirection;
pub use spectrogram::SpectrogramResult;

use ingest::{estimate_bytes, inject_cmd, inject_mzp, try_parse_json_object};

/// Classification under the write lock — recovery of joined text happens after unlock.
enum ResolveOutcome {
    /// Payloads ready to commit (including empty while still buffering).
    Ready(Vec<Value>),
    /// Joined pretty dump to recover outside the lock into one structured/_raw entry.
    Recover(String),
}

pub const DEFAULT_MAX_BYTES: u64 = 1_073_741_824; // 1 GiB
pub const DEFAULT_TTL_HOURS: u64 = 24;
const BROADCAST_CAPACITY: usize = 1024;

/// Result of pushing a line into the store.
#[derive(Debug)]
pub enum PushLineResult {
    /// Zero or more entries emitted (empty while buffering a pretty block).
    Emitted(Vec<LogEntry>),
    /// Service is disconnected; callers should surface HTTP 409.
    Blocked,
}

impl PushLineResult {
    #[cfg(test)]
    pub(crate) fn into_entries(self) -> Vec<LogEntry> {
        match self {
            Self::Emitted(v) => v,
            Self::Blocked => Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn is_blocked(&self) -> bool {
        matches!(self, Self::Blocked)
    }
}

struct Inner {
    entries: VecDeque<LogEntry>,
    approx_bytes: u64,
    max_bytes: u64,
    /// When set, entries older than this age are evicted (oldest-first).
    ttl: Option<Duration>,
    services: HashMap<String, u64>,
    /// Services that must not accept further ingest until reconnected.
    blocked: HashSet<String>,
    /// Global path discovery (session union).
    properties: HashMap<String, PathMeta>,
    /// Per-service path discovery.
    properties_by_service: HashMap<String, HashMap<String, PathMeta>>,
    /// Per-service accumulator for Nest-style pretty `{` … `}` dumps.
    pretty_buffers: HashMap<String, PrettyBuffer>,
    /// Bookmarks / tags / comments (Phase E).
    annotations: HashMap<u64, annotate::Annotation>,
}

pub struct Store {
    inner: RwLock<Inner>,
    next_id: AtomicU64,
    tx: broadcast::Sender<WsEvent>,
    /// Optional durable append writer (Phase K).
    persist: RwLock<Option<persist::PersistWriter>>,
}

impl Store {
    #[cfg(test)]
    pub fn new(max_bytes: u64) -> Self {
        Self::with_ttl_hours(max_bytes, DEFAULT_TTL_HOURS)
    }

    /// Create a store. `ttl_hours == 0` disables age-based eviction (byte cap only).
    pub fn with_ttl_hours(max_bytes: u64, ttl_hours: u64) -> Self {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        let ttl = if ttl_hours == 0 {
            None
        } else {
            Some(Duration::from_secs(ttl_hours.saturating_mul(3600)))
        };
        Self {
            inner: RwLock::new(Inner {
                entries: VecDeque::new(),
                approx_bytes: 0,
                max_bytes: max_bytes.max(1),
                ttl,
                services: HashMap::new(),
                blocked: HashSet::new(),
                properties: HashMap::new(),
                properties_by_service: HashMap::new(),
                pretty_buffers: HashMap::new(),
                annotations: HashMap::new(),
            }),
            next_id: AtomicU64::new(1),
            tx,
            persist: RwLock::new(None),
        }
    }

    /// Drop entries older than the configured TTL. Returns evicted ids (also published).
    pub async fn expire_ttl(&self) -> Vec<u64> {
        let now = Utc::now();
        let evicted = {
            let mut inner = self.inner.write().await;
            Self::evict_expired(&mut inner, now)
        };
        if !evicted.is_empty() {
            self.publish(WsEvent::Evicted {
                ids: evicted.clone(),
            });
            // Keep on-disk persist aligned with the ring.
            self.prune_persist().await;
        }
        evicted
    }

    pub(crate) fn entry_exceeds_ttl(
        received_at: DateTime<Utc>,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> bool {
        now.signed_duration_since(received_at)
            .to_std()
            .is_ok_and(|age| age > ttl)
    }

    fn evict_front(inner: &mut Inner) -> Option<u64> {
        let old = inner.entries.pop_front()?;
        inner.approx_bytes = inner.approx_bytes.saturating_sub(old.approx_bytes);
        if let Some(count) = inner.services.get_mut(&old.service) {
            *count = count.saturating_sub(1);
        }
        decrement_counts_for_entry(&old.data, &mut inner.properties);
        if let Some(svc_map) = inner.properties_by_service.get_mut(&old.service) {
            decrement_counts_for_entry(&old.data, svc_map);
        }
        Some(old.id)
    }

    fn evict_expired(inner: &mut Inner, now: DateTime<Utc>) -> Vec<u64> {
        let Some(ttl) = inner.ttl else {
            return Vec::new();
        };
        let mut evicted_ids = Vec::new();
        while let Some(front) = inner.entries.front() {
            if !Self::entry_exceeds_ttl(front.received_at, ttl, now) {
                break;
            }
            match Self::evict_front(inner) {
                Some(id) => evicted_ids.push(id),
                None => break,
            }
        }
        evicted_ids
    }

    fn evict_over_capacity(inner: &mut Inner) -> Vec<u64> {
        let mut evicted_ids = Vec::new();
        while inner.approx_bytes > inner.max_bytes {
            match Self::evict_front(inner) {
                Some(id) => evicted_ids.push(id),
                None => break,
            }
        }
        evicted_ids
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WsEvent> {
        self.tx.subscribe()
    }

    pub fn publish(&self, event: WsEvent) {
        let _ = self.tx.send(event);
    }

    /// Ingest a line. May emit zero entries (still buffering a pretty block) or one+ entries.
    #[cfg(test)]
    pub async fn push_line(&self, service: &str, line: &str) -> Vec<LogEntry> {
        self.push_line_with_meta(service, line, None, None)
            .await
            .into_entries()
    }

    /// Ingest a line and optionally inject `cmd` / `_mzp` into each emitted payload.
    pub async fn push_line_with_meta(
        &self,
        service: &str,
        line: &str,
        cmd: Option<&str>,
        mzp: Option<&MzpMeta>,
    ) -> PushLineResult {
        self.push_line_with_meta_hint(service, line, cmd, mzp, None)
            .await
    }

    /// Like [`Self::push_line_with_meta`] with an optional locked format hint (file ingest).
    pub async fn push_line_with_meta_hint(
        &self,
        service: &str,
        line: &str,
        cmd: Option<&str>,
        mzp: Option<&MzpMeta>,
        format_hint: Option<&str>,
    ) -> PushLineResult {
        // Classify under the write lock; run recovery outside so large dumps don't stall the hub.
        let cleaned = strip_service_prefix(&strip_ansi(line), service);
        let outcome = {
            let mut inner = self.inner.write().await;
            if inner.blocked.contains(service) {
                return PushLineResult::Blocked;
            }
            Self::resolve_payloads_locked(&mut inner, service, &cleaned, format_hint)
        };
        let payloads = match outcome {
            ResolveOutcome::Ready(v) => v,
            ResolveOutcome::Recover(joined) => {
                vec![Self::recover_and_classify(&joined)]
            }
        };
        let mut emitted = Vec::with_capacity(payloads.len());
        for mut data in payloads {
            if let Some(cmd) = cmd {
                inject_cmd(&mut data, cmd);
            }
            if let Some(mzp) = mzp {
                inject_mzp(&mut data, mzp);
            }
            match self.commit_entry(service, data).await {
                Some(entry) => emitted.push(entry),
                None => return PushLineResult::Blocked,
            }
        }
        PushLineResult::Emitted(emitted)
    }

    /// Whether ingest for `service` is currently blocked (disconnected).
    #[allow(dead_code)] // Store API for callers/tests; used in unit tests below.
    pub async fn is_blocked(&self, service: &str) -> bool {
        let inner = self.inner.read().await;
        inner.blocked.contains(service)
    }

    pub async fn blocked_names(&self) -> Vec<String> {
        let inner = self.inner.read().await;
        let mut names: Vec<String> = inner.blocked.iter().cloned().collect();
        names.sort();
        names
    }

    /// Block ingest for `service`, purge its buffered entries, and broadcast updates.
    pub async fn disconnect_service(&self, service: &str) -> Vec<u64> {
        let (evicted_ids, names, blocked, properties) = {
            let mut inner = self.inner.write().await;
            inner.blocked.insert(service.to_string());
            inner.pretty_buffers.remove(service);
            inner.properties_by_service.remove(service);
            inner.services.remove(service);

            let mut kept = VecDeque::new();
            let mut evicted_ids = Vec::new();
            let mut approx_bytes = 0u64;
            while let Some(entry) = inner.entries.pop_front() {
                if entry.service == service {
                    evicted_ids.push(entry.id);
                } else {
                    approx_bytes += entry.approx_bytes;
                    kept.push_back(entry);
                }
            }
            inner.entries = kept;
            inner.approx_bytes = approx_bytes;

            // Rebuild global + per-service property discovery from remaining entries.
            inner.properties = rebuild_properties_from_entries(&inner.entries);
            inner.properties_by_service = rebuild_properties_by_service(&inner.entries);

            let mut props = paths_to_info(&inner.properties);
            push_service_property(&mut props, &inner.services, None);
            let mut names: Vec<String> = inner.services.keys().cloned().collect();
            names.sort();
            let mut blocked: Vec<String> = inner.blocked.iter().cloned().collect();
            blocked.sort();
            (evicted_ids, names, blocked, props)
        };

        if !evicted_ids.is_empty() {
            self.publish(WsEvent::Evicted {
                ids: evicted_ids.clone(),
            });
        }
        self.publish(WsEvent::Services { names, blocked });
        self.publish(WsEvent::Properties { paths: properties });
        evicted_ids
    }

    /// Allow ingest for a previously disconnected service.
    pub async fn reconnect_service(&self, service: &str) -> bool {
        let removed = {
            let mut inner = self.inner.write().await;
            inner.blocked.remove(service)
        };
        if removed {
            let names = self.service_names().await;
            let blocked = self.blocked_names().await;
            self.publish(WsEvent::Services { names, blocked });
        }
        removed
    }

    async fn publish_services(&self) {
        let names = self.service_names().await;
        let blocked = self.blocked_names().await;
        self.publish(WsEvent::Services { names, blocked });
    }

    /// Recover joined pretty/JS text outside the lock, then apply JSON format packs.
    fn recover_and_classify(joined: &str) -> Value {
        let Some(obj) = recover_json_object(joined) else {
            return joined_raw_payload(joined.to_string());
        };
        if let Value::Object(map) = &obj {
            if let Some(norm) = crate::formats::classify_json_object(map) {
                return norm.data;
            }
            if let Some(norm) = crate::formats::classify_pack_json(map) {
                return norm.data;
            }
        }
        obj
    }

    /// Decide how a cleaned line should be handled (may buffer). Recovery runs after unlock.
    fn resolve_payloads_locked(
        inner: &mut Inner,
        service: &str,
        cleaned: &str,
        format_hint: Option<&str>,
    ) -> ResolveOutcome {
        // Mid pretty-block for this service
        if inner.pretty_buffers.contains_key(service) {
            // Complete single-line JSON interrupts an incomplete pretty dump
            if let Some(obj) = try_parse_json_object(cleaned) {
                let buf = inner
                    .pretty_buffers
                    .remove(service)
                    .expect("pretty buffer present");
                // One joined _raw for the abandoned dump + the interrupting JSON object.
                let mut out = vec![joined_raw_payload(buf.into_joined())];
                out.push(obj);
                return ResolveOutcome::Ready(out);
            }

            let buf = inner
                .pretty_buffers
                .get_mut(service)
                .expect("pretty buffer present");
            buf.push(cleaned.to_string());

            if buf.is_oversized() || buf.is_complete() {
                let buf = inner
                    .pretty_buffers
                    .remove(service)
                    .expect("pretty buffer present");
                return ResolveOutcome::Recover(buf.into_joined());
            }

            return ResolveOutcome::Ready(Vec::new());
        }

        // Not buffering — single-line JSON goes through format packs (bunyan/pino/…).
        if try_parse_json_object(cleaned).is_some() {
            let (payload, _) = crate::formats::parse_ingest_line_with_hint(cleaned, format_hint);
            return ResolveOutcome::Ready(vec![payload]);
        }

        // Single-line JS-literal / recoverables `{ ... }` — recover outside the lock.
        let trimmed = cleaned.trim();
        if trimmed.starts_with('{') && trimmed.ends_with('}') && trimmed.len() > 1 {
            return ResolveOutcome::Recover(trimmed.to_string());
        }
        // Inspect wrappers that are complete on one line (e.g. `Foo { a: 1 }`).
        if crate::pretty_ingest::looks_like_object(trimmed)
            && !is_pretty_block_start(cleaned)
            && trimmed.contains('{')
            && trimmed.ends_with('}')
        {
            return ResolveOutcome::Recover(trimmed.to_string());
        }

        // Start multiline pretty block
        if is_pretty_block_start(cleaned) {
            let buf = PrettyBuffer::start(cleaned.to_string());
            // Enforce byte/line caps on the opener itself (not only on later pushes).
            if buf.is_oversized() {
                return ResolveOutcome::Recover(buf.into_joined());
            }
            inner.pretty_buffers.insert(service.to_string(), buf);
            return ResolveOutcome::Ready(Vec::new());
        }

        // Format detectors (logfmt, syslog, access_log, packs, …) then raw
        let (payload, _) = crate::formats::parse_ingest_line_with_hint(cleaned, format_hint);
        ResolveOutcome::Ready(vec![payload])
    }

    /// Commit one entry. Returns `None` if the service became blocked.
    async fn commit_entry(&self, service: &str, mut data: Value) -> Option<LogEntry> {
        redact::redact_value(&mut data);
        let approx_bytes = estimate_bytes(service, &data);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let received_at = Utc::now();
        let event_time = crate::event_time::extract_event_time(&data).or(Some(received_at));
        let format_id = data
            .as_object()
            .and_then(|o| o.get("_format"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                if data.get("_raw").is_some()
                    && data.as_object().map(|o| o.len() == 1) == Some(true)
                {
                    Some("raw".into())
                } else {
                    Some("json".into())
                }
            });
        let entry = LogEntry {
            id,
            received_at,
            event_time,
            service: service.to_string(),
            format_id,
            data,
            approx_bytes,
        };

        let (evicted, services_changed, properties) = {
            let mut inner = self.inner.write().await;
            if inner.blocked.contains(service) {
                return None;
            }
            let service_was_new = !inner.services.contains_key(service);
            *inner.services.entry(service.to_string()).or_insert(0) += 1;

            let mut schema_changed =
                discover_paths_into(&entry.data, "", &mut inner.properties, true);
            let service_props = inner
                .properties_by_service
                .entry(service.to_string())
                .or_default();
            schema_changed |= discover_paths_into(&entry.data, "", service_props, true);

            inner.approx_bytes += entry.approx_bytes;
            inner.entries.push_back(entry.clone());

            let mut evicted_ids = Self::evict_expired(&mut inner, entry.received_at);
            evicted_ids.extend(Self::evict_over_capacity(&mut inner));

            let props = if schema_changed || service_was_new {
                let mut props = paths_to_info(&inner.properties);
                push_service_property(&mut props, &inner.services, None);
                Some(props)
            } else {
                None
            };
            (evicted_ids, service_was_new, props)
        };

        if !evicted.is_empty() {
            self.publish(WsEvent::Evicted {
                ids: evicted.clone(),
            });
        }
        if services_changed {
            self.publish_services().await;
        }
        if let Some(properties) = properties {
            self.publish(WsEvent::Properties { paths: properties });
        }
        self.publish(WsEvent::Log {
            entry: entry.clone(),
        });
        self.persist_entry(&entry).await;

        Some(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::CompiledQuery;
    use chrono::Duration as ChronoDuration;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn reassembles_pretty_multiline_object() {
        let store = Store::new(1_000_000);
        assert!(store.push_line("api", "{").await.is_empty());
        assert!(store.push_line("api", "  level: 'info',").await.is_empty());
        assert!(store
            .push_line("api", "  message: 'hello',")
            .await
            .is_empty());
        let entries = store.push_line("api", "}").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].data["level"], json!("info"));
        assert_eq!(entries[0].data["message"], json!("hello"));
    }

    #[tokio::test]
    async fn ndjson_unchanged() {
        let store = Store::new(1_000_000);
        let entries = store
            .push_line("api", r#"{"level":"warn","msg":"x"}"#)
            .await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].data["level"], json!("warn"));
    }

    #[tokio::test]
    async fn injects_cmd_into_json_and_raw() {
        let store = Store::new(1_000_000);
        let json_entries = store
            .push_line_with_meta(
                "/Users/me/app",
                r#"{"level":"info","msg":"hi","cmd":"from-app"}"#,
                Some("npm test"),
                None,
            )
            .await
            .into_entries();
        assert_eq!(json_entries.len(), 1);
        assert_eq!(json_entries[0].data["cmd"], json!("npm test"));
        assert_eq!(json_entries[0].data["msg"], json!("hi"));

        let raw_entries = store
            .push_line_with_meta("/Users/me/app", "plain text", Some("cargo run"), None)
            .await
            .into_entries();
        assert_eq!(raw_entries.len(), 1);
        assert_eq!(raw_entries[0].data["_raw"], json!("plain text"));
        assert_eq!(raw_entries[0].data["cmd"], json!("cargo run"));
    }

    #[tokio::test]
    async fn injects_mzp_into_json_and_raw() {
        let store = Store::new(1_000_000);
        let mzp = MzpMeta {
            cwd: "/Users/me/app".into(),
            user: "me".into(),
            pid: 4242,
            exe: "/usr/local/bin/mzp".into(),
        };
        let json_entries = store
            .push_line_with_meta(
                "/Users/me/app",
                r#"{"level":"info","msg":"hi","_mzp":{"cwd":"from-app"}}"#,
                None,
                Some(&mzp),
            )
            .await
            .into_entries();
        assert_eq!(json_entries.len(), 1);
        assert_eq!(json_entries[0].data["msg"], json!("hi"));
        assert_eq!(json_entries[0].data["_mzp"], json!(mzp));

        let raw_entries = store
            .push_line_with_meta("/Users/me/app", "plain text", None, Some(&mzp))
            .await
            .into_entries();
        assert_eq!(raw_entries.len(), 1);
        assert_eq!(raw_entries[0].data["_raw"], json!("plain text"));
        assert_eq!(raw_entries[0].data["_mzp"], json!(mzp));
    }

    #[tokio::test]
    async fn failed_pretty_flushes_as_single_raw() {
        let store = Store::new(1_000_000);
        assert!(store.push_line("api", "{").await.is_empty());
        assert!(store.push_line("api", "  !!!not valid!!!").await.is_empty());
        let entries = store.push_line("api", "}").await;
        // One joined _raw — never explode into per-line rows.
        assert_eq!(entries.len(), 1);
        assert!(entries[0].data.get("_raw").is_some());
        let raw = entries[0].data["_raw"].as_str().unwrap();
        assert!(raw.contains("!!!not valid!!!"));
    }

    #[tokio::test]
    async fn json_interrupts_pretty_buffer() {
        let store = Store::new(1_000_000);
        assert!(store.push_line("api", "{").await.is_empty());
        assert!(store.push_line("api", "  a: 1,").await.is_empty());
        let entries = store.push_line("api", r#"{"level":"info"}"#).await;
        // One joined _raw for abandoned dump + the JSON object
        assert_eq!(entries.len(), 2);
        assert!(entries[0].data.get("_raw").is_some());
        assert_eq!(entries[1].data["level"], json!("info"));
    }

    #[tokio::test]
    async fn strips_service_prefix_for_pretty_block() {
        let store = Store::new(1_000_000);
        assert!(store.push_line("api", "[api] {").await.is_empty());
        assert!(store
            .push_line("api", "[api]   context: { context: 'bootstrap' },")
            .await
            .is_empty());
        assert!(store
            .push_line("api", "[api]   level: 'info',")
            .await
            .is_empty());
        assert!(store
            .push_line(
                "api",
                "[api]   message: 'Application is running on: http://localhost:3000/api',"
            )
            .await
            .is_empty());
        assert!(store
            .push_line("api", "[api]   timestamp: '2026-07-15T19:47:22.775Z',")
            .await
            .is_empty());
        assert!(store
            .push_line("api", "[api]   ms: '+0ms'")
            .await
            .is_empty());
        let entries = store.push_line("api", "[api] }").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].data["level"], json!("info"));
        assert_eq!(
            entries[0].data["message"],
            json!("Application is running on: http://localhost:3000/api")
        );
        assert_eq!(entries[0].data["ms"], json!("+0ms"));
        assert_eq!(entries[0].data["context"]["context"], json!("bootstrap"));
    }

    #[tokio::test]
    async fn strips_process_manager_prefix_when_service_is_cwd() {
        // Shell attach defaults service to absolute cwd; concurrently still prefixes [api].
        let service = "/Users/lucas/Documents/GitHub/monorepo";
        let store = Store::new(1_000_000);
        assert!(store.push_line(service, "[api] {").await.is_empty());
        assert!(store
            .push_line(service, "[api]   context: { context: 'bootstrap' },")
            .await
            .is_empty());
        assert!(store
            .push_line(service, "[api]   level: 'info',")
            .await
            .is_empty());
        assert!(store
            .push_line(
                service,
                "[api]   message: 'Application is running on: http://localhost:3000/api',"
            )
            .await
            .is_empty());
        assert!(store
            .push_line(service, "[api]   timestamp: '2026-07-15T19:47:22.775Z',")
            .await
            .is_empty());
        assert!(store
            .push_line(service, "[api]   ms: '+0ms'")
            .await
            .is_empty());
        let entries = store.push_line(service, "[api] }").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].data["level"], json!("info"));
        assert_eq!(
            entries[0].data["message"],
            json!("Application is running on: http://localhost:3000/api")
        );
        assert_eq!(entries[0].data["ms"], json!("+0ms"));
        assert_eq!(entries[0].data["context"]["context"], json!("bootstrap"));
    }

    #[tokio::test]
    async fn foreign_process_prefix_starts_pretty_block() {
        let store = Store::new(1_000_000);
        assert!(store.push_line("api", "[other] {").await.is_empty());
        assert!(store
            .push_line("api", "[other]   level: 'warn',")
            .await
            .is_empty());
        let entries = store.push_line("api", "[other] }").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].data["level"], json!("warn"));
    }

    #[tokio::test]
    async fn search_properties_filters_and_counts() {
        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"level":"error","msg":"boom","user":{"id":"1"}}"#)
            .await;
        store
            .push_line("api", r#"{"level":"info","msg":"ok","user":{"id":"2"}}"#)
            .await;
        store
            .push_line("web", r#"{"level":"error","msg":"fail"}"#)
            .await;

        let all = store.search_properties(None, None).await;
        let level = all.iter().find(|p| p.path == "level").expect("level");
        assert_eq!(level.count, 3);
        let error = level
            .values
            .iter()
            .find(|v| v.value == "error")
            .expect("error sample");
        assert_eq!(error.count, 2);

        let service = all.iter().find(|p| p.path == "service").expect("service");
        assert_eq!(service.count, 3);
        assert!(service.sample_values.iter().any(|v| v == "api"));
        assert!(service.sample_values.iter().any(|v| v == "web"));

        let nested = all.iter().find(|p| p.path == "user.id").expect("user.id");
        assert_eq!(nested.count, 2);

        let filtered = store.search_properties(None, Some("erro")).await;
        assert!(filtered.iter().any(|p| p.path == "level"));
        let level = filtered.iter().find(|p| p.path == "level").unwrap();
        assert_eq!(level.sample_values, vec!["error".to_string()]);
        assert!(!filtered.iter().any(|p| p.path == "msg"));

        let by_path = store.search_properties(None, Some("user")).await;
        assert!(by_path.iter().any(|p| p.path == "user.id"));
    }

    #[tokio::test]
    async fn activity_histogram_buckets_last_window() {
        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"level":"info","msg":"now"}"#)
            .await;
        let buckets = store
            .activity_histogram(ChronoDuration::hours(24), ChronoDuration::minutes(25))
            .await;
        assert_eq!(buckets.len(), 58); // ceil(24h / 25m)
        let total: u64 = buckets.iter().map(|b| b.count).sum();
        assert_eq!(total, 1);
        assert_eq!(buckets.last().unwrap().count, 1);
    }

    #[tokio::test]
    async fn query_logs_respects_time_range() {
        let store = Store::new(1_000_000);
        store.push_line("api", r#"{"msg":"hi"}"#).await;
        let (all, _) = store
            .query_logs(None, None, 10, &CompiledQuery::MatchAll, None, None)
            .await;
        assert_eq!(all.len(), 1);
        let ts = all[0].received_at;
        let (none, _) = store
            .query_logs(
                None,
                None,
                10,
                &CompiledQuery::MatchAll,
                Some(ts + ChronoDuration::seconds(1)),
                None,
            )
            .await;
        assert!(none.is_empty());
        let (one, _) = store
            .query_logs(
                None,
                None,
                10,
                &CompiledQuery::MatchAll,
                Some(ts - ChronoDuration::seconds(1)),
                Some(ts + ChronoDuration::seconds(1)),
            )
            .await;
        assert_eq!(one.len(), 1);
    }

    #[tokio::test]
    async fn properties_ws_only_on_schema_change() {
        let store = Store::new(1_000_000);
        let mut rx = store.subscribe();
        store
            .push_line("api", r#"{"level":"info","msg":"a"}"#)
            .await;
        let mut saw_props = false;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, WsEvent::Properties { .. }) {
                saw_props = true;
            }
        }
        assert!(saw_props, "first ingest should publish properties");

        // Same paths/types/samples — only the log event should fire.
        store
            .push_line("api", r#"{"level":"info","msg":"a"}"#)
            .await;
        let mut saw_props_again = false;
        let mut saw_log = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                WsEvent::Properties { .. } => saw_props_again = true,
                WsEvent::Log { .. } => saw_log = true,
                _ => {}
            }
        }
        assert!(saw_log);
        assert!(
            !saw_props_again,
            "same schema should not rebroadcast properties"
        );

        store
            .push_line("api", r#"{"level":"info","msg":"c","extra":1}"#)
            .await;
        let mut saw_new_props = false;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, WsEvent::Properties { .. }) {
                saw_new_props = true;
            }
        }
        assert!(saw_new_props, "new field should publish properties");
    }

    #[tokio::test]
    async fn concurrent_ingest_and_query() {
        let store = Arc::new(Store::new(1_000_000));
        let mut handles = Vec::new();
        for i in 0..8 {
            let store = Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                for j in 0..20 {
                    store
                        .push_line("api", &format!(r#"{{"worker":{i},"n":{j}}}"#))
                        .await;
                }
            }));
        }
        let store_q = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            for _ in 0..10 {
                let _ = store_q
                    .query_logs(None, None, 5, &CompiledQuery::MatchAll, None, None)
                    .await;
                let _ = store_q.search_properties(None, None).await;
            }
        }));
        for h in handles {
            h.await.expect("task");
        }
        let stats = store.stats().await;
        assert_eq!(stats.count, 160);
    }

    #[tokio::test]
    async fn blocked_push_returns_blocked_variant() {
        let store = Store::new(1_000_000);
        store.push_line("api", r#"{"msg":"hi"}"#).await;
        store.disconnect_service("api").await;
        assert!(store
            .push_line_with_meta("api", r#"{"msg":"nope"}"#, None, None)
            .await
            .is_blocked());
    }

    #[tokio::test]
    async fn disconnect_blocks_ingest_and_purges() {
        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"level":"error","msg":"boom"}"#)
            .await;
        store
            .push_line("web", r#"{"level":"info","msg":"ok"}"#)
            .await;

        let evicted = store.disconnect_service("api").await;
        assert_eq!(evicted.len(), 1);
        assert!(store.is_blocked("api").await);
        assert_eq!(store.service_names().await, vec!["web".to_string()]);
        assert_eq!(store.blocked_names().await, vec!["api".to_string()]);

        let stats = store.stats().await;
        assert_eq!(stats.count, 1);
        assert_eq!(stats.services.len(), 1);
        assert_eq!(stats.services[0].name, "web");

        // Blocked ingest is a no-op.
        assert!(store
            .push_line("api", r#"{"level":"error","msg":"again"}"#)
            .await
            .is_empty());
        assert_eq!(store.stats().await.count, 1);

        assert!(store.reconnect_service("api").await);
        assert!(!store.is_blocked("api").await);
        let entries = store
            .push_line("api", r#"{"level":"info","msg":"back"}"#)
            .await;
        assert_eq!(entries.len(), 1);
        assert_eq!(store.stats().await.count, 2);
    }

    #[tokio::test]
    async fn ttl_evicts_old_entries_on_ingest() {
        let store = Store::with_ttl_hours(1_000_000, 1);
        store.push_line("api", r#"{"msg":"old"}"#).await;
        store.push_line("api", r#"{"msg":"also-old"}"#).await;
        {
            let mut inner = store.inner.write().await;
            let cutoff = Utc::now() - ChronoDuration::hours(2);
            for entry in inner.entries.iter_mut() {
                entry.received_at = cutoff;
            }
        }
        let mut rx = store.subscribe();
        store.push_line("api", r#"{"msg":"fresh"}"#).await;
        assert_eq!(store.stats().await.count, 1);
        let mut evicted = false;
        while let Ok(ev) = rx.try_recv() {
            if let WsEvent::Evicted { ids } = ev {
                assert_eq!(ids.len(), 2);
                evicted = true;
            }
        }
        assert!(evicted, "TTL eviction should broadcast Evicted");
    }

    #[tokio::test]
    async fn expire_ttl_drops_stale_without_ingest() {
        let store = Store::with_ttl_hours(1_000_000, 1);
        store.push_line("api", r#"{"msg":"stale"}"#).await;
        {
            let mut inner = store.inner.write().await;
            inner.entries[0].received_at = Utc::now() - ChronoDuration::hours(2);
        }
        let ids = store.expire_ttl().await;
        assert_eq!(ids.len(), 1);
        assert_eq!(store.stats().await.count, 0);
    }

    #[tokio::test]
    async fn ttl_zero_disables_age_eviction() {
        let store = Store::with_ttl_hours(1_000_000, 0);
        store.push_line("api", r#"{"msg":"ancient"}"#).await;
        {
            let mut inner = store.inner.write().await;
            inner.entries[0].received_at = Utc::now() - ChronoDuration::hours(48);
        }
        assert!(store.expire_ttl().await.is_empty());
        store.push_line("api", r#"{"msg":"new"}"#).await;
        assert_eq!(store.stats().await.count, 2);
    }

    #[tokio::test]
    async fn evicts_oldest_when_over_byte_capacity() {
        let store = Store::new(180);
        store.push_line("api", r#"{"msg":"first"}"#).await;
        store
            .push_line("api", r#"{"msg":"second-with-more-bytes"}"#)
            .await;
        assert_eq!(store.stats().await.count, 1);
    }

    #[tokio::test]
    async fn oversized_pretty_buffer_recovers_or_single_raw() {
        let store = Store::new(50_000_000);
        assert!(store.push_line("api", "{").await.is_empty());
        // Exceed the 1 MiB byte cap with one huge incomplete line.
        let huge = format!("  x: '{}',", "a".repeat(1024 * 1024));
        let entries = store.push_line("api", &huge).await;
        assert_eq!(entries.len(), 1);
        // Incomplete dump can't recover to a closed object → single joined _raw.
        assert!(entries[0].data.get("_raw").is_some());
    }

    #[tokio::test]
    async fn oversized_pretty_opener_recovers_immediately() {
        let store = Store::new(50_000_000);
        // First line alone exceeds the 1 MiB byte cap while still incomplete.
        let huge = format!("{{ type: '{}',", "a".repeat(1024 * 1024));
        let entries = store.push_line("api", &huge).await;
        assert_eq!(entries.len(), 1);
        assert!(entries[0].data.get("_raw").is_some());
    }

    #[tokio::test]
    async fn js_control_flow_does_not_start_pretty_buffer() {
        let store = Store::new(1_000_000);
        let entries = store.push_line("api", "try {").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].data["_raw"], json!("try {"));
        // Following lines must still emit as their own raw entries (not buffered).
        let entries = store.push_line("api", "doWork();").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].data["_raw"], json!("doWork();"));
        let entries = store.push_line("api", "return { a: 1 }").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].data["_raw"], json!("return { a: 1 }"));
    }

    #[tokio::test]
    async fn large_evidence_decision_pretty_dump_is_one_structured_entry() {
        let store = Store::new(50_000_000);
        assert!(store.push_line("api", "{").await.is_empty());
        assert!(store
            .push_line(
                "api",
                "  type: 'inventory.relationships.evidence.decision',"
            )
            .await
            .is_empty());
        assert!(store.push_line("api", "  level: 'info',").await.is_empty());
        assert!(store.push_line("api", "  evidence: [").await.is_empty());
        for i in 0..300 {
            assert!(store
                .push_line("api", &format!("    {{ id: {i}, ok: true }},"))
                .await
                .is_empty());
        }
        assert!(store.push_line("api", "  ],").await.is_empty());
        assert!(store
            .push_line("api", "  message: 'done',")
            .await
            .is_empty());
        let entries = store.push_line("api", "}").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].data["type"],
            json!("inventory.relationships.evidence.decision")
        );
        assert_eq!(entries[0].data["message"], json!("done"));
        assert_eq!(entries[0].data["evidence"].as_array().unwrap().len(), 300);
    }

    #[tokio::test]
    async fn incomplete_opener_starts_pretty_buffer() {
        let store = Store::new(1_000_000);
        assert!(store
            .push_line(
                "api",
                "{ type: 'inventory.relationships.evidence.decision',"
            )
            .await
            .is_empty());
        assert!(store.push_line("api", "  level: 'info',").await.is_empty());
        let entries = store.push_line("api", "}").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].data["type"],
            json!("inventory.relationships.evidence.decision")
        );
        assert_eq!(entries[0].data["level"], json!("info"));
    }

    #[tokio::test]
    async fn single_line_js_object_ingests() {
        let store = Store::new(1_000_000);
        let entries = store.push_line("api", "{ level: 'info', msg: 'hi' }").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].data["level"], json!("info"));
        assert_eq!(entries[0].data["msg"], json!("hi"));
    }

    #[tokio::test]
    async fn reconnect_on_unknown_service_is_false() {
        let store = Store::new(1_000_000);
        assert!(!store.reconnect_service("missing").await);
    }
}
