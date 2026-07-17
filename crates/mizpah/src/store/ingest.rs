//! Line parsing and size estimation helpers for ingest.

use super::MzpMeta;
use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

pub(crate) fn in_time_range(
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
pub(crate) fn inject_cmd(data: &mut Value, cmd: &str) {
    if let Value::Object(map) = data {
        map.insert("cmd".to_string(), Value::String(cmd.to_string()));
    }
}

/// Inject or overwrite top-level `_mzp` receiver metadata on a log payload object.
pub(crate) fn inject_mzp(data: &mut Value, mzp: &MzpMeta) {
    if let Value::Object(map) = data {
        if let Ok(value) = serde_json::to_value(mzp) {
            map.insert("_mzp".to_string(), value);
        }
    }
}

pub(crate) fn try_parse_json_object(line: &str) -> Option<Value> {
    let trimmed = line.trim();
    match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::Object(obj)) => Some(Value::Object(obj)),
        _ => None,
    }
}

pub(crate) fn raw_payloads_from_lines(lines: Vec<String>) -> Vec<Value> {
    lines
        .into_iter()
        .map(|line| Value::Object(Map::from_iter([("_raw".to_string(), Value::String(line))])))
        .collect()
}

pub(crate) fn estimate_bytes(service: &str, data: &Value) -> u64 {
    let json_len = estimate_json_len(data);
    let overhead = 64 + service.len() as u64;
    json_len + overhead
}

/// Approximate serialized JSON size without allocating a full string.
pub(crate) fn estimate_json_len(value: &Value) -> u64 {
    match value {
        Value::Null => 4,
        Value::Bool(true) => 4,
        Value::Bool(false) => 5,
        Value::Number(n) => n.to_string().len() as u64,
        Value::String(s) => 2 + s.len() as u64,
        Value::Array(arr) => {
            let mut n = 2u64; // []
            for (i, v) in arr.iter().enumerate() {
                if i > 0 {
                    n += 1;
                }
                n += estimate_json_len(v);
            }
            n
        }
        Value::Object(map) => {
            let mut n = 2u64; // {}
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    n += 1;
                }
                n += 3 + k.len() as u64; // "key":
                n += estimate_json_len(v);
            }
            n
        }
    }
}
