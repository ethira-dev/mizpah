use crate::pretty_ingest::{
    is_pretty_block_start, parse_pretty_block, strip_ansi, strip_service_prefix, PrettyBuffer,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{broadcast, RwLock};

pub const DEFAULT_MAX_BYTES: u64 = 1_073_741_824; // 1 GiB
const BROADCAST_CAPACITY: usize = 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub id: u64,
    pub received_at: DateTime<Utc>,
    pub service: String,
    pub data: Value,
    #[serde(skip)]
    pub approx_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PropertyInfo {
    pub path: String,
    pub types: Vec<String>,
    pub sample_values: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceInfo {
    pub name: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub count: u64,
    pub approx_bytes: u64,
    pub max_bytes: u64,
    pub services: Vec<ServiceInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WsEvent {
    #[serde(rename = "log")]
    Log { entry: LogEntry },
    #[serde(rename = "evicted")]
    Evicted { ids: Vec<u64> },
    #[serde(rename = "services")]
    Services { names: Vec<String> },
    #[serde(rename = "properties")]
    Properties { paths: Vec<PropertyInfo> },
}

#[derive(Debug, Clone, Default)]
struct PathMeta {
    types: HashSet<String>,
    sample_values: Vec<String>,
}

struct Inner {
    entries: VecDeque<LogEntry>,
    approx_bytes: u64,
    max_bytes: u64,
    services: HashMap<String, u64>,
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
    pub async fn push_line(&self, service: &str, line: &str) -> Vec<LogEntry> {
        let cleaned = strip_service_prefix(&strip_ansi(line), service);
        let payloads = self.resolve_payloads(service, &cleaned).await;
        let mut emitted = Vec::with_capacity(payloads.len());
        for data in payloads {
            emitted.push(self.commit_entry(service, data).await);
        }
        emitted
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
            inner
                .pretty_buffers
                .insert(service.to_string(), PrettyBuffer::start(cleaned.to_string()));
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

            let props = paths_to_info(&inner.properties);
            (evicted_ids, service_was_new, props)
        };

        if !evicted.is_empty() {
            self.publish(WsEvent::Evicted {
                ids: evicted.clone(),
            });
        }
        if services_changed {
            let names = self.service_names().await;
            self.publish(WsEvent::Services { names });
        }
        self.publish(WsEvent::Properties {
            paths: properties,
        });
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

    pub async fn properties(&self, service: Option<&str>) -> Vec<PropertyInfo> {
        let inner = self.inner.read().await;
        match service {
            Some(svc) if !svc.is_empty() && svc != "*" => {
                if let Some(map) = inner.properties_by_service.get(svc) {
                    paths_to_info(map)
                } else {
                    Vec::new()
                }
            }
            _ => paths_to_info(&inner.properties),
        }
    }

    pub async fn query_logs(
        &self,
        service: Option<&str>,
        cursor: Option<u64>,
        limit: usize,
        filters: &[crate::filter::FilterChip],
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
            if !crate::filter::matches_all(&entry.service, &entry.data, filters) {
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
        Ok(other) => Value::Object(Map::from_iter([(
            "_value".to_string(),
            other,
        )])),
        Err(_) => Value::Object(Map::from_iter([(
            "_raw".to_string(),
            Value::String(trimmed.to_string()),
        )])),
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
        .map(|line| {
            Value::Object(Map::from_iter([(
                "_raw".to_string(),
                Value::String(line),
            )]))
        })
        .collect()
}

fn estimate_bytes(service: &str, data: &Value) -> u64 {
    let json_len = data.to_string().len() as u64;
    let overhead = 64 + service.len() as u64;
    json_len + overhead
}

fn discover_paths_into(value: &Value, prefix: &str, map: &mut HashMap<String, PathMeta>) {
    match value {
        Value::Object(obj) => {
            for (key, child) in obj {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                record_path(&path, child, map);
                if child.is_object() {
                    discover_paths_into(child, &path, map);
                } else if let Value::Array(arr) = child {
                    for (i, item) in arr.iter().enumerate().take(5) {
                        if item.is_object() {
                            let item_path = format!("{path}[{i}]");
                            discover_paths_into(item, &item_path, map);
                        }
                    }
                }
            }
        }
        other => {
            if !prefix.is_empty() {
                record_path(prefix, other, map);
            }
        }
    }
}

fn record_path(path: &str, value: &Value, map: &mut HashMap<String, PathMeta>) {
    let meta = map.entry(path.to_string()).or_default();
    let type_name = match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) => {
            if n.is_f64() && !n.is_i64() && !n.is_u64() {
                "number"
            } else {
                "number"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    meta.types.insert(type_name.to_string());

    if meta.sample_values.len() < 20 {
        let sample = match value {
            Value::String(s) => {
                if s.len() > 80 {
                    format!("{}…", &s[..80])
                } else {
                    s.clone()
                }
            }
            Value::Null => "null".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            Value::Array(_) | Value::Object(_) => return,
        };
        if !meta.sample_values.contains(&sample) {
            meta.sample_values.push(sample);
        }
    }
}

fn paths_to_info(map: &HashMap<String, PathMeta>) -> Vec<PropertyInfo> {
    let mut infos: Vec<PropertyInfo> = map
        .iter()
        .map(|(path, meta)| {
            let mut types: Vec<String> = meta.types.iter().cloned().collect();
            types.sort();
            PropertyInfo {
                path: path.clone(),
                types,
                sample_values: meta.sample_values.clone(),
            }
        })
        .collect();
    infos.sort_by(|a, b| a.path.cmp(&b.path));
    infos
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn reassembles_pretty_multiline_object() {
        let store = Store::new(1_000_000);
        assert!(store.push_line("api", "{").await.is_empty());
        assert!(store
            .push_line("api", "  level: 'info',")
            .await
            .is_empty());
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
        let entries = store
            .push_line("api", r#"{"level":"info"}"#)
            .await;
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
    async fn foreign_service_prefix_not_stripped() {
        let store = Store::new(1_000_000);
        let entries = store.push_line("api", "[other] {").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].data["_raw"], json!("[other] {"));
    }
}
