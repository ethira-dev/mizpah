//! Token-efficient MCP result formatting (slim + TOON).

use serde::Serialize;
use toon_format::encode_default;

use crate::mcp::client::{LogsResponse, PropertiesResponse};

/// Strip noisy `_mzp` metadata from each log entry's `data` object.
pub fn slim_logs(mut resp: LogsResponse) -> LogsResponse {
    for entry in &mut resp.entries {
        if let Some(obj) = entry.as_object_mut() {
            if let Some(data) = obj.get_mut("data").and_then(|d| d.as_object_mut()) {
                data.remove("_mzp");
            }
        }
    }
    resp
}

/// Drop redundant `sampleValues` when `values` already carries the same samples.
pub fn slim_properties(mut resp: PropertiesResponse) -> PropertiesResponse {
    for prop in &mut resp.properties {
        let Some(obj) = prop.as_object_mut() else {
            continue;
        };
        let has_values = obj
            .get("values")
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty());
        if has_values {
            obj.remove("sampleValues");
        }
    }
    resp
}

/// Encode a value as TOON for MCP tool results.
///
/// Falls back to compact JSON if TOON encoding fails (should be rare).
pub fn encode_mcp_value(value: &impl Serialize) -> String {
    match encode_default(value) {
        Ok(text) => text,
        Err(_) => serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string()),
    }
}

/// Slim + encode a logs response for MCP.
pub fn format_logs(resp: LogsResponse) -> String {
    encode_mcp_value(&slim_logs(resp))
}

/// Slim + encode a properties response for MCP.
pub fn format_properties(resp: PropertiesResponse) -> String {
    encode_mcp_value(&slim_properties(resp))
}

/// Encode any other MCP payload (stats, services) as TOON.
pub fn format_value(value: &impl Serialize) -> String {
    encode_mcp_value(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::LogEntry;
    use serde_json::{json, Value};
    use toon_format::decode_default;

    fn sample_logs() -> LogsResponse {
        LogsResponse {
            entries: vec![
                json!({
                    "id": 42,
                    "receivedAt": "2026-07-17T00:00:00Z",
                    "service": "api",
                    "data": {
                        "level": "error",
                        "msg": "timeout",
                        "_mzp": {
                            "cwd": "/Users/me/app",
                            "user": "lucas",
                            "pid": 12345,
                            "exe": "/usr/local/bin/mzp"
                        }
                    }
                }),
                json!({
                    "id": 41,
                    "receivedAt": "2026-07-17T00:00:01Z",
                    "service": "api",
                    "data": {
                        "level": "info",
                        "msg": "ok",
                        "_mzp": {
                            "cwd": "/Users/me/app",
                            "user": "lucas",
                            "pid": 12345,
                            "exe": "/usr/local/bin/mzp"
                        }
                    }
                }),
            ],
            has_more: true,
        }
    }

    fn sample_properties() -> PropertiesResponse {
        PropertiesResponse {
            properties: vec![json!({
                "path": "level",
                "types": ["string"],
                "sampleValues": ["error", "info"],
                "count": 3,
                "values": [
                    { "value": "error", "count": 2 },
                    { "value": "info", "count": 1 }
                ]
            })],
        }
    }

    #[test]
    fn slim_logs_strips_mzp_keeps_other_fields() {
        let slimmed = slim_logs(sample_logs());
        assert_eq!(slimmed.entries.len(), 2);
        for entry in &slimmed.entries {
            let data = entry["data"].as_object().expect("data object");
            assert!(!data.contains_key("_mzp"));
            assert!(data.contains_key("level"));
            assert!(data.contains_key("msg"));
        }
        assert_eq!(slimmed.entries[0]["id"], 42);
        assert_eq!(slimmed.entries[0]["service"], "api");
        assert!(slimmed.has_more);
    }

    #[test]
    fn slim_properties_omits_sample_values_when_values_present() {
        let slimmed = slim_properties(sample_properties());
        let prop = &slimmed.properties[0];
        assert!(prop.get("sampleValues").is_none());
        assert_eq!(prop["values"].as_array().unwrap().len(), 2);
        assert_eq!(prop["path"], "level");
        assert_eq!(prop["count"], 3);
    }

    #[test]
    fn slim_properties_keeps_sample_values_when_values_empty() {
        let resp = PropertiesResponse {
            properties: vec![json!({
                "path": "msg",
                "types": ["string"],
                "sampleValues": ["hi"],
                "count": 1,
                "values": []
            })],
        };
        let slimmed = slim_properties(resp);
        assert_eq!(slimmed.properties[0]["sampleValues"], json!(["hi"]));
    }

    #[test]
    fn toon_encode_logs_succeeds_with_markers() {
        let text = format_logs(sample_logs());
        assert!(
            text.contains("entries["),
            "expected tabular entries marker, got:\n{text}"
        );
        assert!(text.contains("hasMore:"), "expected hasMore, got:\n{text}");
        assert!(
            !text.contains("_mzp"),
            "expected _mzp stripped, got:\n{text}"
        );
    }

    #[test]
    fn toon_encode_properties_succeeds() {
        let text = format_properties(sample_properties());
        assert!(text.contains("properties["), "got:\n{text}");
        assert!(text.contains("level"), "got:\n{text}");
        assert!(!text.contains("sampleValues"), "got:\n{text}");
    }

    #[test]
    fn toon_encode_stats_and_services() {
        let stats = json!({
            "count": 42,
            "approxBytes": 12345,
            "maxBytes": 1073741824,
            "services": [
                { "name": "api", "count": 40 },
                { "name": "web", "count": 2 }
            ]
        });
        let services = json!({
            "services": ["api", "web"],
            "blocked": ["old"]
        });
        let stats_text = format_value(&stats);
        let services_text = format_value(&services);
        assert!(stats_text.contains("count:"), "got:\n{stats_text}");
        assert!(services_text.contains("services["), "got:\n{services_text}");
    }

    #[test]
    fn toon_round_trip_preserves_slimmed_logs() {
        let slimmed = slim_logs(sample_logs());
        let expected = serde_json::to_value(&slimmed).unwrap();
        let text = encode_mcp_value(&slimmed);
        let decoded: Value = decode_default(&text).expect("decode TOON");
        assert_eq!(decoded, expected);
    }

    #[test]
    fn toon_round_trip_preserves_slimmed_properties() {
        let slimmed = slim_properties(sample_properties());
        let expected = serde_json::to_value(&slimmed).unwrap();
        let text = encode_mcp_value(&slimmed);
        let decoded: Value = decode_default(&text).expect("decode TOON");
        assert_eq!(decoded, expected);
    }

    #[test]
    fn toon_is_denser_than_pretty_json() {
        let slimmed = slim_logs(sample_logs());
        let toon = encode_mcp_value(&slimmed);
        let pretty = serde_json::to_string_pretty(&slimmed).unwrap();
        assert!(
            toon.len() < pretty.len(),
            "TOON ({} bytes) should be smaller than pretty JSON ({} bytes)\nTOON:\n{toon}\nJSON:\n{pretty}",
            toon.len(),
            pretty.len()
        );
    }

    #[test]
    fn fixture_log_entry_formats_without_mzp() {
        let raw = include_str!("../../tests/fixtures/log_entry.json");
        let entry: LogEntry = serde_json::from_str(raw).expect("fixture");
        let mut value = serde_json::to_value(&entry).unwrap();
        value["data"]["_mzp"] = json!({
            "cwd": "/tmp",
            "user": "test",
            "pid": 1,
            "exe": "mzp"
        });
        let resp = LogsResponse {
            entries: vec![value],
            has_more: false,
        };
        let text = format_logs(resp);
        assert!(
            text.contains("timeout") || text.contains("error"),
            "got:\n{text}"
        );
        assert!(!text.contains("_mzp"), "got:\n{text}");
        assert!(text.contains("hasMore:"), "got:\n{text}");
    }
}
