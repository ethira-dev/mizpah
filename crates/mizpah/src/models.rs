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
