//! Extract wall-clock event time from structured log payloads.

use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::Value;

/// Common field names checked (first match wins).
const FIELD_NAMES: &[&str] = &[
    "timestamp",
    "@timestamp",
    "time",
    "ts",
    "datetime",
    "date",
    "eventTime",
    "event_time",
    "logged_at",
    "loggedAt",
];

/// Parse an event timestamp from `data`, or return `None` if absent/unparseable.
pub fn extract_event_time(data: &Value) -> Option<DateTime<Utc>> {
    let obj = data.as_object()?;
    for name in FIELD_NAMES {
        if let Some(v) = obj.get(*name) {
            if let Some(dt) = parse_time_value(v) {
                return Some(dt);
            }
        }
    }
    None
}

fn parse_time_value(v: &Value) -> Option<DateTime<Utc>> {
    match v {
        Value::String(s) => parse_time_str(s.trim()),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                return parse_epoch(i);
            }
            if let Some(f) = n.as_f64() {
                return parse_epoch_f64(f);
            }
            None
        }
        _ => None,
    }
}

fn parse_epoch(n: i64) -> Option<DateTime<Utc>> {
    // Heuristic: ms vs seconds
    if n.abs() >= 1_000_000_000_000 {
        DateTime::from_timestamp_millis(n)
    } else {
        DateTime::from_timestamp(n, 0)
    }
}

fn parse_epoch_f64(f: f64) -> Option<DateTime<Utc>> {
    if !f.is_finite() {
        return None;
    }
    if f.abs() >= 1_000_000_000_000.0 {
        DateTime::from_timestamp_millis(f as i64)
    } else {
        let secs = f.trunc() as i64;
        let nsecs = ((f.fract()) * 1_000_000_000.0).abs() as u32;
        DateTime::from_timestamp(secs, nsecs)
    }
}

fn parse_time_str(s: &str) -> Option<DateTime<Utc>> {
    if s.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    // Common variants without timezone → assume UTC
    const NAIVE_FMTS: &[&str] = &[
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y/%m/%d %H:%M:%S",
    ];
    for fmt in NAIVE_FMTS {
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(DateTime::from_naive_utc_and_offset(naive, Utc));
        }
    }
    // Numeric string epoch
    if let Ok(n) = s.parse::<i64>() {
        return parse_epoch(n);
    }
    if let Ok(f) = s.parse::<f64>() {
        return parse_epoch_f64(f);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_rfc3339_timestamp() {
        let data = json!({"timestamp": "2024-01-15T12:00:00Z", "msg": "hi"});
        let dt = extract_event_time(&data).unwrap();
        assert_eq!(dt.to_rfc3339(), "2024-01-15T12:00:00+00:00");
    }

    #[test]
    fn extracts_at_timestamp() {
        let data = json!({"@timestamp": "2024-06-01T08:30:00.123Z"});
        assert!(extract_event_time(&data).is_some());
    }

    #[test]
    fn extracts_epoch_millis() {
        let data = json!({"ts": 1_704_067_200_000i64}); // 2024-01-01T00:00:00Z
        let dt = extract_event_time(&data).unwrap();
        assert_eq!(dt.timestamp(), 1_704_067_200);
    }

    #[test]
    fn missing_returns_none() {
        let data = json!({"msg": "no time"});
        assert!(extract_event_time(&data).is_none());
    }

    #[test]
    fn alternate_fields_and_formats() {
        let time_field = json!({"time": "2024-01-01 00:00:00", "msg": "x"});
        assert!(extract_event_time(&time_field).is_some());

        let ts_secs = json!({"ts": 1_704_067_200i64});
        assert_eq!(
            extract_event_time(&ts_secs).unwrap().timestamp(),
            1_704_067_200
        );

        let ts_float = json!({"ts": 1_704_067_200.5});
        assert!(extract_event_time(&ts_float).is_some());

        let bad = json!({"ts": f64::NAN});
        assert!(extract_event_time(&bad).is_none());

        let empty = json!({"timestamp": ""});
        assert!(extract_event_time(&empty).is_none());

        assert!(extract_event_time(&json!("not object")).is_none());

        let numeric_str = json!({"datetime": "1704067200"});
        assert!(extract_event_time(&numeric_str).is_some());

        let event_time = json!({"eventTime": "2024-01-15T12:00:00Z"});
        assert!(extract_event_time(&event_time).is_some());

        let slash = json!({"date": "2024/01/15 12:00:00"});
        assert!(extract_event_time(&slash).is_some());

        let logged_at = json!({"logged_at": "2024-01-15T12:00:00Z"});
        assert!(extract_event_time(&logged_at).is_some());

        let logged_at_camel = json!({"loggedAt": "2024-01-15T12:00:00Z"});
        assert!(extract_event_time(&logged_at_camel).is_some());

        let bad_then_good = json!({
            "timestamp": "not-a-date",
            "time": "2024-01-01 00:00:00"
        });
        assert!(extract_event_time(&bad_then_good).is_some());

        let event_time = json!({"event_time": "2024-01-15T12:00:00Z"});
        assert!(extract_event_time(&event_time).is_some());

        let naive_frac = json!({"time": "2024-01-01T00:00:00.123"});
        assert!(extract_event_time(&naive_frac).is_some());

        let float_ms = json!({"ts": 1_704_067_200_000.0});
        assert_eq!(
            extract_event_time(&float_ms).unwrap().timestamp(),
            1_704_067_200
        );
    }

    #[test]
    fn first_matching_field_wins() {
        let data = json!({
            "timestamp": "2024-01-01T00:00:00Z",
            "time": "2025-01-01T00:00:00Z"
        });
        assert_eq!(
            extract_event_time(&data).unwrap().to_rfc3339(),
            "2024-01-01T00:00:00+00:00"
        );
    }
}
