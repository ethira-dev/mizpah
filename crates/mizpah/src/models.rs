use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
pub struct PropertyValueInfo {
    pub value: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PropertyInfo {
    pub path: String,
    pub types: Vec<String>,
    pub sample_values: Vec<String>,
    /// Entries in the buffer that currently have this field. `0` when not computed
    /// (e.g. lightweight WebSocket property snapshots).
    #[serde(default)]
    pub count: u64,
    /// Sample values with occurrence counts. Empty when counts were not computed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<PropertyValueInfo>,
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
#[serde(rename_all = "camelCase")]
pub struct ActivityBucket {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WsEvent {
    #[serde(rename = "log")]
    Log { entry: LogEntry },
    #[serde(rename = "evicted")]
    Evicted { ids: Vec<u64> },
    #[serde(rename = "services")]
    Services {
        names: Vec<String>,
        #[serde(default)]
        blocked: Vec<String>,
    },
    #[serde(rename = "properties")]
    Properties { paths: Vec<PropertyInfo> },
    /// Heartbeat reply to a client `ping`.
    #[serde(rename = "pong")]
    Pong,
    /// Broadcast subscriber fell behind; client should resync via REST.
    #[serde(rename = "lagged")]
    Lagged {
        /// Number of broadcast messages dropped for this subscriber.
        skipped: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lagged_event_serializes_for_clients() {
        let ev = WsEvent::Lagged { skipped: 42 };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v, json!({"type": "lagged", "skipped": 42}));
    }

    #[test]
    fn fixture_log_entry_roundtrip() {
        let raw = include_str!("../tests/fixtures/log_entry.json");
        let entry: LogEntry = serde_json::from_str(raw).expect("log_entry fixture");
        assert_eq!(entry.id, 42);
        assert_eq!(entry.service, "api");
        let again = serde_json::to_value(&entry).unwrap();
        assert_eq!(again["receivedAt"], "2026-07-17T00:00:00Z");
        assert_eq!(again["data"]["msg"], "timeout");
    }

    #[test]
    fn fixture_ws_events_deserialize() {
        let raw = include_str!("../tests/fixtures/ws_events.json");
        let events: Vec<WsEvent> = serde_json::from_str(raw).expect("ws_events fixture");
        assert_eq!(events.len(), 6);
        assert!(matches!(events[0], WsEvent::Log { .. }));
        assert!(matches!(events[1], WsEvent::Evicted { .. }));
        assert!(matches!(events[2], WsEvent::Services { .. }));
        assert!(matches!(events[3], WsEvent::Properties { .. }));
        assert!(matches!(events[4], WsEvent::Pong));
        assert!(matches!(events[5], WsEvent::Lagged { skipped: 7 }));
    }
}
