use crate::pretty_ingest::{
    is_pretty_block_start, parse_pretty_block, strip_ansi, strip_service_prefix, PrettyBuffer,
};
use crate::properties::{
    annotate_property_counts, discover_paths_into, filter_properties_by_query, paths_to_info,
    push_service_property, PathMeta,
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{broadcast, RwLock};

// Re-export public DTOs so existing `crate::store::{...}` imports keep working.
pub use crate::models::{ActivityBucket, LogEntry, PropertyInfo, ServiceInfo, Stats, WsEvent};
pub use crate::mzp_meta::MzpMeta;

pub const DEFAULT_MAX_BYTES: u64 = 1_073_741_824; // 1 GiB
const BROADCAST_CAPACITY: usize = 1024;

struct Inner {
    entries: VecDeque<LogEntry>,
    approx_bytes: u64,
    max_bytes: u64,
    services: HashMap<String, u64>,
    /// Services that must not accept further ingest until reconnected.
    blocked: HashSet<String>,
    /// Global path discovery (session union).
    properties: HashMap<String, PathMeta>,
    /// Per-service path discovery.
    properties_by_service: HashMap<String, HashMap<String, PathMeta>>,
    /// Per-service accumulator for Nest-style pretty `{` … `}` dumps.
    pretty_buffers: HashMap<String, PrettyBuffer>,
}

pub struct Store {
    inner: RwLock<Inner>,
    next_id: AtomicU64,
    tx: broadcast::Sender<WsEvent>,
}

impl Store {
    pub fn new(max_bytes: u64) -> Self {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            inner: RwLock::new(Inner {
                entries: VecDeque::new(),
                approx_bytes: 0,
                max_bytes: max_bytes.max(1),
                services: HashMap::new(),
                blocked: HashSet::new(),
                properties: HashMap::new(),
                properties_by_service: HashMap::new(),
                pretty_buffers: HashMap::new(),
            }),
            next_id: AtomicU64::new(1),
            tx,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WsEvent> {
        self.tx.subscribe()
    }

    pub fn publish(&self, event: WsEvent) {
        let _ = self.tx.send(event);
    }

    /// Ingest a line. May emit zero entries (still buffering a pretty block),
    /// one entry (normal / completed block), or many (failed convert flush).
    #[cfg(test)]
    pub async fn push_line(&self, service: &str, line: &str) -> Vec<LogEntry> {
        self.push_line_with_meta(service, line, None, None).await
    }

    /// Ingest a line and optionally inject `cmd` / `_mzp` into each emitted payload.
    /// Returns an empty vec when the service is blocked (callers should treat HTTP ingest as 409).
    pub async fn push_line_with_meta(
        &self,
        service: &str,
        line: &str,
        cmd: Option<&str>,
        mzp: Option<&MzpMeta>,
    ) -> Vec<LogEntry> {
        if self.is_blocked(service).await {
            return Vec::new();
        }
        let cleaned = strip_service_prefix(&strip_ansi(line), service);
        let payloads = self.resolve_payloads(service, &cleaned).await;
        let mut emitted = Vec::with_capacity(payloads.len());
        for mut data in payloads {
            if let Some(cmd) = cmd {
                inject_cmd(&mut data, cmd);
            }
            if let Some(mzp) = mzp {
                inject_mzp(&mut data, mzp);
            }
            emitted.push(self.commit_entry(service, data).await);
        }
        emitted
    }

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

            // Rebuild global property discovery from remaining entries.
            let mut properties = HashMap::new();
            for entry in &inner.entries {
                discover_paths_into(&entry.data, "", &mut properties);
            }
            inner.properties = properties;

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

    /// Decide which JSON payloads a cleaned line should become (may buffer).
    async fn resolve_payloads(&self, service: &str, cleaned: &str) -> Vec<Value> {
        let mut inner = self.inner.write().await;

        // Mid pretty-block for this service
        if inner.pretty_buffers.contains_key(service) {
            // Complete single-line JSON interrupts an incomplete pretty dump
            if try_parse_json_object(cleaned).is_some() {
                let buf = inner.pretty_buffers.remove(service).unwrap();
                let mut out = raw_payloads_from_lines(buf.into_lines());
                if let Some(obj) = try_parse_json_object(cleaned) {
                    out.push(obj);
                }
                return out;
            }

            let buf = inner.pretty_buffers.get_mut(service).unwrap();
            buf.push(cleaned.to_string());

            if buf.is_oversized() {
                let buf = inner.pretty_buffers.remove(service).unwrap();
                return raw_payloads_from_lines(buf.into_lines());
            }

            if buf.is_complete() {
                let buf = inner.pretty_buffers.remove(service).unwrap();
                if let Some(obj) = parse_pretty_block(&buf.joined()) {
                    return vec![obj];
                }
                return raw_payloads_from_lines(buf.into_lines());
            }

            return Vec::new();
        }

        // Not buffering — prefer single-line JSON
        if let Some(obj) = try_parse_json_object(cleaned) {
            return vec![obj];
        }

        // Single-line JS-literal object `{ ... }`
        let trimmed = cleaned.trim();
        if trimmed.starts_with('{') && trimmed.ends_with('}') && trimmed.len() > 1 {
            if let Some(obj) = parse_pretty_block(trimmed) {
                return vec![obj];
            }
        }

        // Start multiline pretty block
        if is_pretty_block_start(cleaned) {
            inner.pretty_buffers.insert(
                service.to_string(),
                PrettyBuffer::start(cleaned.to_string()),
            );
            return Vec::new();
        }

        vec![parse_line(cleaned)]
    }

    async fn commit_entry(&self, service: &str, data: Value) -> LogEntry {
        let approx_bytes = estimate_bytes(service, &data);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let entry = LogEntry {
            id,
            received_at: Utc::now(),
            service: service.to_string(),
            data,
            approx_bytes,
        };

        let (evicted, services_changed, properties) = {
            let mut inner = self.inner.write().await;
            let service_was_new = !inner.services.contains_key(service);
            *inner.services.entry(service.to_string()).or_insert(0) += 1;

            discover_paths_into(&entry.data, "", &mut inner.properties);
            let service_props = inner
                .properties_by_service
                .entry(service.to_string())
                .or_default();
            discover_paths_into(&entry.data, "", service_props);

            inner.approx_bytes += entry.approx_bytes;
            inner.entries.push_back(entry.clone());

            let mut evicted_ids = Vec::new();
            while inner.approx_bytes > inner.max_bytes {
                if let Some(old) = inner.entries.pop_front() {
                    inner.approx_bytes = inner.approx_bytes.saturating_sub(old.approx_bytes);
                    if let Some(count) = inner.services.get_mut(&old.service) {
                        *count = count.saturating_sub(1);
                    }
                    evicted_ids.push(old.id);
                } else {
                    break;
                }
            }

            let mut props = paths_to_info(&inner.properties);
            push_service_property(&mut props, &inner.services, None);
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
        self.publish(WsEvent::Properties { paths: properties });
        self.publish(WsEvent::Log {
            entry: entry.clone(),
        });

        entry
    }

    pub async fn service_names(&self) -> Vec<String> {
        let inner = self.inner.read().await;
        let mut names: Vec<String> = inner.services.keys().cloned().collect();
        names.sort();
        names
    }

    pub async fn stats(&self) -> Stats {
        let inner = self.inner.read().await;
        let mut services: Vec<ServiceInfo> = inner
            .services
            .iter()
            .map(|(name, count)| ServiceInfo {
                name: name.clone(),
                count: *count,
            })
            .collect();
        services.sort_by(|a, b| a.name.cmp(&b.name));
        Stats {
            count: inner.entries.len() as u64,
            approx_bytes: inner.approx_bytes,
            max_bytes: inner.max_bytes,
            services,
        }
    }

    /// Search and list discovered fields with occurrence counts over the full buffer.
    ///
    /// - `q` matches property paths or sample values (case-insensitive substring).
    /// - When only values match, non-matching samples are dropped from the result.
    /// - Always includes a synthetic `service` field when services are present.
    pub async fn search_properties(
        &self,
        service: Option<&str>,
        q: Option<&str>,
    ) -> Vec<PropertyInfo> {
        let inner = self.inner.read().await;
        let mut infos = match service {
            Some(svc) if !svc.is_empty() && svc != "*" => {
                if let Some(map) = inner.properties_by_service.get(svc) {
                    paths_to_info(map)
                } else {
                    Vec::new()
                }
            }
            _ => paths_to_info(&inner.properties),
        };
        push_service_property(&mut infos, &inner.services, service);

        let needle = q
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_lowercase());

        if let Some(ref needle) = needle {
            infos = filter_properties_by_query(infos, needle);
        }

        // Sort sample values alphabetically before counting so UI order is stable.
        for info in &mut infos {
            info.sample_values.sort_by_key(|a| a.to_ascii_lowercase());
        }

        annotate_property_counts(&inner.entries, service, &mut infos);
        infos
    }

    pub async fn get_entry(&self, id: u64) -> Option<LogEntry> {
        let inner = self.inner.read().await;
        inner.entries.iter().find(|e| e.id == id).cloned()
    }

    pub async fn query_logs(
        &self,
        service: Option<&str>,
        cursor: Option<u64>,
        limit: usize,
        query: &crate::filter::CompiledQuery,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> (Vec<LogEntry>, bool) {
        let inner = self.inner.read().await;
        let limit = limit.clamp(1, 500);
        let mut matched = Vec::new();
        let mut has_more = false;

        // Newest first
        for entry in inner.entries.iter().rev() {
            if let Some(c) = cursor {
                if entry.id >= c {
                    continue;
                }
            }
            if let Some(svc) = service {
                if !svc.is_empty() && svc != "*" && entry.service != svc {
                    continue;
                }
            }
            if !in_time_range(entry.received_at, from, to) {
                continue;
            }
            if !crate::filter::matches_entry(&entry.service, &entry.data, query) {
                continue;
            }
            if matched.len() >= limit {
                has_more = true;
                break;
            }
            matched.push(entry.clone());
        }

        (matched, has_more)
    }

    /// Bucket entry counts over a trailing window (oldest → newest).
    pub async fn activity_histogram(
        &self,
        window: ChronoDuration,
        bucket: ChronoDuration,
    ) -> Vec<ActivityBucket> {
        let window = if window <= ChronoDuration::zero() {
            ChronoDuration::hours(24)
        } else {
            window
        };
        let bucket = if bucket <= ChronoDuration::zero() {
            ChronoDuration::minutes(25)
        } else {
            bucket
        };

        let now = Utc::now();
        let bucket_ms = bucket.num_milliseconds().max(1);
        let window_ms = window.num_milliseconds().max(1);
        let n_buckets = ((window_ms + bucket_ms - 1) / bucket_ms) as usize;
        let n_buckets = n_buckets.max(1);

        // Align to a fixed UTC grid so bucket boundaries stay stable across polls.
        let now_ms = now.timestamp_millis();
        let current_bucket_end = ((now_ms / bucket_ms) + 1) * bucket_ms;
        let window_start_ms = current_bucket_end - bucket_ms * n_buckets as i64;
        let window_start =
            DateTime::from_timestamp_millis(window_start_ms).unwrap_or_else(|| now - window);

        let mut counts = vec![0u64; n_buckets];
        {
            let inner = self.inner.read().await;
            for entry in &inner.entries {
                let ts_ms = entry.received_at.timestamp_millis();
                if ts_ms < window_start_ms {
                    continue;
                }
                let offset = ts_ms - window_start_ms;
                let idx = (offset / bucket_ms) as usize;
                if idx < n_buckets {
                    counts[idx] += 1;
                }
            }
        }

        counts
            .into_iter()
            .enumerate()
            .map(|(i, count)| {
                let start = window_start + ChronoDuration::milliseconds(bucket_ms * i as i64);
                let end = start + ChronoDuration::milliseconds(bucket_ms);
                ActivityBucket { start, end, count }
            })
            .collect()
    }
}

fn in_time_range(
    ts: DateTime<Utc>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> bool {
    if let Some(from) = from {
        if ts < from {
            return false;
        }
    }
    if let Some(to) = to {
        if ts >= to {
            return false;
        }
    }
    true
}

pub fn parse_line(line: &str) -> Value {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Value::Object(Map::from_iter([(
            "_raw".to_string(),
            Value::String(String::new()),
        )]));
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::Object(obj)) => Value::Object(obj),
        Ok(other) => Value::Object(Map::from_iter([("_value".to_string(), other)])),
        Err(_) => Value::Object(Map::from_iter([(
            "_raw".to_string(),
            Value::String(trimmed.to_string()),
        )])),
    }
}

/// Inject or overwrite top-level `cmd` on a log payload object.
fn inject_cmd(data: &mut Value, cmd: &str) {
    if let Value::Object(map) = data {
        map.insert("cmd".to_string(), Value::String(cmd.to_string()));
    }
}

/// Inject or overwrite top-level `_mzp` receiver metadata on a log payload object.
fn inject_mzp(data: &mut Value, mzp: &MzpMeta) {
    if let Value::Object(map) = data {
        if let Ok(value) = serde_json::to_value(mzp) {
            map.insert("_mzp".to_string(), value);
        }
    }
}

fn try_parse_json_object(line: &str) -> Option<Value> {
    let trimmed = line.trim();
    match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::Object(obj)) => Some(Value::Object(obj)),
        _ => None,
    }
}

fn raw_payloads_from_lines(lines: Vec<String>) -> Vec<Value> {
    lines
        .into_iter()
        .map(|line| Value::Object(Map::from_iter([("_raw".to_string(), Value::String(line))])))
        .collect()
}

fn estimate_bytes(service: &str, data: &Value) -> u64 {
    let json_len = data.to_string().len() as u64;
    let overhead = 64 + service.len() as u64;
    json_len + overhead
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::CompiledQuery;
    use serde_json::json;

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
            .await;
        assert_eq!(json_entries.len(), 1);
        assert_eq!(json_entries[0].data["cmd"], json!("npm test"));
        assert_eq!(json_entries[0].data["msg"], json!("hi"));

        let raw_entries = store
            .push_line_with_meta("/Users/me/app", "plain text", Some("cargo run"), None)
            .await;
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
            .await;
        assert_eq!(json_entries.len(), 1);
        assert_eq!(json_entries[0].data["msg"], json!("hi"));
        assert_eq!(json_entries[0].data["_mzp"], json!(mzp));

        let raw_entries = store
            .push_line_with_meta("/Users/me/app", "plain text", None, Some(&mzp))
            .await;
        assert_eq!(raw_entries.len(), 1);
        assert_eq!(raw_entries[0].data["_raw"], json!("plain text"));
        assert_eq!(raw_entries[0].data["_mzp"], json!(mzp));
    }

    #[tokio::test]
    async fn failed_pretty_flushes_as_raw() {
        let store = Store::new(1_000_000);
        assert!(store.push_line("api", "{").await.is_empty());
        assert!(store.push_line("api", "  !!!not valid!!!").await.is_empty());
        let entries = store.push_line("api", "}").await;
        assert_eq!(entries.len(), 3);
        assert!(entries.iter().all(|e| e.data.get("_raw").is_some()));
    }

    #[tokio::test]
    async fn json_interrupts_pretty_buffer() {
        let store = Store::new(1_000_000);
        assert!(store.push_line("api", "{").await.is_empty());
        assert!(store.push_line("api", "  a: 1,").await.is_empty());
        let entries = store.push_line("api", r#"{"level":"info"}"#).await;
        // flushed pretty lines as raw + the JSON object
        assert!(entries.len() >= 2);
        assert_eq!(entries.last().unwrap().data["level"], json!("info"));
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
}
