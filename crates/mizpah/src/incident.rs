//! Incident / "what broke?" summary over a recent time window.

use crate::filter::CompiledQuery;
use crate::store::Store;
use chrono::{Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LevelCount {
    pub level: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceCount {
    pub service: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSample {
    pub msg: String,
    pub count: u64,
    pub sample_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceCount {
    pub opid: String,
    pub error_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncidentSummary {
    pub minutes: u64,
    pub total: u64,
    pub by_level: Vec<LevelCount>,
    pub top_services: Vec<ServiceCount>,
    pub top_messages: Vec<MessageSample>,
    pub top_traces: Vec<TraceCount>,
    pub notes: Vec<String>,
}

/// Summarize recent buffer activity for the last `minutes`.
pub async fn summarize_incident(store: &Store, minutes: u64) -> IncidentSummary {
    let minutes = minutes.clamp(1, 24 * 60);
    let to = Utc::now();
    let from = to - ChronoDuration::minutes(minutes as i64);

    let (entries, _) = store
        .query_logs(
            None,
            None,
            500,
            &CompiledQuery::MatchAll,
            Some(from),
            Some(to),
        )
        .await;

    let total = entries.len() as u64;
    let mut level_map: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut svc_map: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut msg_map: std::collections::HashMap<String, (u64, Option<u64>)> =
        std::collections::HashMap::new();

    for e in &entries {
        let level = e
            .data
            .get("level")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_ascii_lowercase();
        *level_map.entry(level).or_default() += 1;
        *svc_map.entry(e.service.clone()).or_default() += 1;
        let msg = e
            .data
            .get("msg")
            .and_then(|v| v.as_str())
            .or_else(|| e.data.get("_raw").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        if !msg.is_empty() {
            let slot = msg_map.entry(msg).or_insert((0, Some(e.id)));
            slot.0 += 1;
        }
    }

    let mut by_level: Vec<LevelCount> = level_map
        .into_iter()
        .map(|(level, count)| LevelCount { level, count })
        .collect();
    by_level.sort_by(|a, b| b.count.cmp(&a.count));

    let mut top_services: Vec<ServiceCount> = svc_map
        .into_iter()
        .map(|(service, count)| ServiceCount { service, count })
        .collect();
    top_services.sort_by(|a, b| b.count.cmp(&a.count));
    top_services.truncate(10);

    let mut top_messages: Vec<MessageSample> = msg_map
        .into_iter()
        .map(|(msg, (count, sample_id))| MessageSample {
            msg,
            count,
            sample_id,
        })
        .collect();
    top_messages.sort_by(|a, b| b.count.cmp(&a.count));
    top_messages.truncate(10);

    let traces = store.list_traces(20).await;
    let top_traces: Vec<TraceCount> = traces
        .into_iter()
        .map(|t| TraceCount {
            opid: t.opid,
            error_count: t.error_count,
        })
        .collect();

    let mut notes = Vec::new();
    if total == 0 {
        notes.push("No logs in this window. Pipe or upload logs, or widen --minutes.".into());
    }

    IncidentSummary {
        minutes,
        total,
        by_level,
        top_services,
        top_messages,
        top_traces,
        notes,
    }
}
