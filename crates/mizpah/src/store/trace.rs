//! Trace / opid grouping helpers.

use super::{LogEntry, Store};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceSummary {
    pub opid: String,
    pub count: u64,
    pub min_event_time: DateTime<Utc>,
    pub max_event_time: DateTime<Utc>,
    pub error_count: u64,
    pub warn_count: u64,
}

fn classify_level(data: &Value) -> &'static str {
    let level = data
        .get("level")
        .or_else(|| data.get("severity"))
        .or_else(|| data.get("lvl"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if level == "error" || level == "err" || level == "fatal" || level == "critical" {
        "error"
    } else if level == "warn" || level == "warning" {
        "warn"
    } else {
        "other"
    }
}

/// Resolve an opid / trace id from a log payload using configured field names.
pub fn resolve_opid(data: &Value, field_names: &[String]) -> Option<String> {
    let obj = data.as_object()?;
    for name in field_names {
        if let Some(v) = obj.get(name) {
            let s = match v {
                Value::String(s) if !s.is_empty() => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => continue,
            };
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

impl Store {
    /// All entries sharing `opid`, oldest-first, capped at `limit`.
    pub async fn get_trace(&self, opid: &str, limit: usize) -> Vec<LogEntry> {
        let limit = limit.clamp(1, 500);
        let fields = crate::config::MizpahConfig::load().trace_fields;
        let inner = self.inner.read().await;
        let mut matched: Vec<LogEntry> = inner
            .entries
            .iter()
            .filter(|e| resolve_opid(&e.data, &fields).as_deref() == Some(opid))
            .cloned()
            .collect();
        matched.sort_by_key(|e| (e.effective_event_time(), e.id));
        matched.truncate(limit);
        matched
    }

    /// Distinct traces currently in the buffer, newest activity first.
    pub async fn list_traces(&self, limit: usize) -> Vec<TraceSummary> {
        let limit = limit.clamp(1, 200);
        let fields = crate::config::MizpahConfig::load().trace_fields;
        let mut map: HashMap<String, TraceSummary> = HashMap::new();
        {
            let inner = self.inner.read().await;
            for entry in &inner.entries {
                let Some(opid) = resolve_opid(&entry.data, &fields) else {
                    continue;
                };
                let t = entry.effective_event_time();
                let summary = map.entry(opid.clone()).or_insert_with(|| TraceSummary {
                    opid,
                    count: 0,
                    min_event_time: t,
                    max_event_time: t,
                    error_count: 0,
                    warn_count: 0,
                });
                summary.count += 1;
                if t < summary.min_event_time {
                    summary.min_event_time = t;
                }
                if t > summary.max_event_time {
                    summary.max_event_time = t;
                }
                match classify_level(&entry.data) {
                    "error" => summary.error_count += 1,
                    "warn" => summary.warn_count += 1,
                    _ => {}
                }
            }
        }
        let mut out: Vec<TraceSummary> = map.into_values().collect();
        out.sort_by_key(|s| std::cmp::Reverse(s.max_event_time));
        out.truncate(limit);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use serde_json::json;

    #[test]
    fn resolve_opid_prefers_configured_fields() {
        let data = json!({"traceId": "abc", "request_id": "req-1"});
        let fields = vec!["request_id".into(), "traceId".into()];
        assert_eq!(resolve_opid(&data, &fields).as_deref(), Some("req-1"));
    }

    #[tokio::test]
    async fn get_trace_oldest_first() {
        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"trace_id":"t1","msg":"a","level":"info"}"#)
            .await;
        store
            .push_line("api", r#"{"trace_id":"t1","msg":"b","level":"error"}"#)
            .await;
        store
            .push_line("api", r#"{"trace_id":"t2","msg":"other"}"#)
            .await;
        let rows = store.get_trace("t1", 50).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].data["msg"], json!("a"));
        assert_eq!(rows[1].data["msg"], json!("b"));

        let summaries = store.list_traces(10).await;
        let t1 = summaries.iter().find(|s| s.opid == "t1").unwrap();
        assert_eq!(t1.count, 2);
        assert_eq!(t1.error_count, 1);
    }

    #[test]
    fn resolve_opid_numeric_empty_and_non_object() {
        assert_eq!(resolve_opid(&json!(123), &["id".into()]), None);
        assert_eq!(
            resolve_opid(&json!({"id": ""}), &["id".into()]).as_deref(),
            None
        );
        assert_eq!(resolve_opid(&json!({"id": true}), &["id".into()]), None);
        assert_eq!(
            resolve_opid(&json!({"id": 42}), &["id".into()]).as_deref(),
            Some("42")
        );
    }

    #[tokio::test]
    async fn list_traces_tracks_min_max_event_time() {
        let store = Store::new(1_000_000);
        store
            .push_line(
                "api",
                r#"{"trace_id":"t1","msg":"early","@timestamp":"2024-01-01T00:00:00Z"}"#,
            )
            .await;
        store
            .push_line(
                "api",
                r#"{"trace_id":"t1","msg":"late","@timestamp":"2024-06-01T00:00:00Z"}"#,
            )
            .await;
        let summaries = store.list_traces(10).await;
        let t1 = summaries.iter().find(|s| s.opid == "t1").unwrap();
        assert_eq!(t1.count, 2);
        assert!(t1.min_event_time < t1.max_event_time);
    }

    #[tokio::test]
    async fn list_traces_counts_warn_levels() {
        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"trace_id":"t1","level":"warn"}"#)
            .await;
        let summaries = store.list_traces(10).await;
        assert_eq!(summaries[0].warn_count, 1);
    }

    #[tokio::test]
    async fn list_traces_skips_entries_without_opid() {
        let store = Store::new(1_000_000);
        store.push_line("api", r#"{"msg":"no trace"}"#).await;
        store
            .push_line("api", r#"{"trace_id":"t1","msg":"has trace"}"#)
            .await;
        let summaries = store.list_traces(10).await;
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].opid, "t1");
    }
}
