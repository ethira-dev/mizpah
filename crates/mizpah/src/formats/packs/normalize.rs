//! Field normalization from pack captures / JSON packs into Mizpah shape.

use serde_json::{json, Map, Value};

/// Stable Mizpah ids for formats we already emit; new packs keep their pack ids.
pub fn mizpah_format_id(pack_id: &str) -> &str {
    match pack_id {
        "syslog_log" => "syslog",
        "bunyan_log" => "bunyan",
        "pino_log" => "pino",
        "journald_json_log" => "journald",
        "access_log" => "access_log",
        "slog_json_log" => "slog",
        "zerolog_json_log" => "zerolog",
        "logrus_json_log" => "logrus",
        "structlog_json_log" => "structlog",
        other => other,
    }
}

/// Packs that Mizpah already handles via hand parsers / JSON field packs.
/// Still loaded for sample tests, but not used as primary winners in the pipe path.
pub fn is_mizpah_primary_pack(pack_id: &str) -> bool {
    matches!(
        pack_id,
        "syslog_log"
            | "access_log"
            | "bunyan_log"
            | "pino_log"
            | "journald_json_log"
            | "slog_json_log"
            | "zerolog_json_log"
            | "logrus_json_log"
            | "structlog_json_log"
    )
}

/// Resolve `a/b/c` or `a.b` style paths used in format packs.
pub fn json_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(root);
    }
    // Prefer exact top-level key (e.g. `@timestamp`, `log.level` as literal key).
    if let Some(obj) = root.as_object() {
        if let Some(v) = obj.get(path) {
            return Some(v);
        }
    }
    let sep = if path.contains('/') { '/' } else { '.' };
    let mut cur = root;
    for part in path.split(sep) {
        if part.is_empty() {
            continue;
        }
        cur = cur.as_object()?.get(part)?;
    }
    Some(cur)
}

fn normalize_level_str(raw: &str) -> String {
    let lower = raw.trim().to_ascii_lowercase();
    match lower.as_str() {
        "warning" | "warn" => "warn".into(),
        "err" => "error".into(),
        "fatal" | "critical" | "crit" | "emergency" | "alert" => {
            if lower == "critical" || lower == "crit" {
                "critical".into()
            } else if lower.starts_with("fatal") {
                "fatal".into()
            } else {
                "error".into()
            }
        }
        "information" => "info".into(),
        other => other.to_string(),
    }
}

/// Map numeric / string level using optional pack `level` table (string patterns or numbers).
pub fn map_level(raw: &Value, level_map: &Map<String, Value>) -> Option<String> {
    // Numeric Bunyan-style: level map values are numbers matching raw.
    if let Some(n) = raw.as_i64().or_else(|| raw.as_u64().map(|u| u as i64)) {
        for (name, v) in level_map {
            if v.as_i64() == Some(n) || v.as_u64().map(|u| u as i64) == Some(n) {
                return Some(normalize_level_str(name));
            }
            if let Some(s) = v.as_str() {
                if s.parse::<i64>().ok() == Some(n) {
                    return Some(normalize_level_str(name));
                }
            }
        }
        return Some(n.to_string());
    }
    let s = match raw {
        Value::String(s) => s.as_str(),
        other => return Some(normalize_level_str(&other.to_string())),
    };
    // Regex patterns as map values (text level rules) — try contains / full match loosely.
    for (name, v) in level_map {
        if let Some(pat) = v.as_str() {
            if pat.starts_with('^') || pat.contains("(?:") || pat.contains('[') {
                if let Ok(re) = regex::Regex::new(pat) {
                    if re.is_match(s) {
                        return Some(normalize_level_str(name));
                    }
                }
            } else if s.eq_ignore_ascii_case(pat) || s.eq_ignore_ascii_case(name) {
                return Some(normalize_level_str(name));
            }
        }
    }
    Some(normalize_level_str(s))
}

/// Apply Mizpah field aliases onto a capture map.
pub fn apply_text_aliases(map: &mut Map<String, Value>) {
    // body → msg
    if !map.contains_key("msg") {
        if let Some(v) = map.get("body").cloned() {
            map.insert("msg".into(), v);
        } else if let Some(v) = map.get("message").cloned() {
            map.insert("msg".into(), v);
        }
    }
    // timestamp variants → @timestamp when missing
    if !map.contains_key("@timestamp") && !map.contains_key("timestamp") {
        for key in ["timestamp", "ts", "time", "t", "__timestamp__"] {
            if let Some(v) = map.get(key).cloned() {
                map.insert("@timestamp".into(), v);
                break;
            }
        }
    } else if map.contains_key("timestamp") && !map.contains_key("@timestamp") {
        if let Some(v) = map.get("timestamp").cloned() {
            map.insert("@timestamp".into(), v);
        }
    }

    // access_log-ish helpers
    if let Some(Value::String(req)) = map.get("request").cloned() {
        if !map.contains_key("method") || !map.contains_key("path") {
            let mut parts = req.split_whitespace();
            if let Some(m) = parts.next() {
                map.entry("method".to_string()).or_insert_with(|| json!(m));
                if let Some(p) = parts.next() {
                    map.entry("path".to_string()).or_insert_with(|| json!(p));
                }
            }
        }
    }
    if let Some(status) = map.get("status").cloned() {
        let code = status
            .as_u64()
            .or_else(|| status.as_i64().map(|i| i as u64))
            .or_else(|| status.as_str().and_then(|s| s.parse().ok()));
        if let Some(code) = code {
            map.insert("status".into(), json!(code));
            if !map.contains_key("level") {
                let level = if code >= 500 {
                    "error"
                } else if code >= 400 {
                    "warn"
                } else {
                    "info"
                };
                map.insert("level".into(), json!(level));
            }
        }
    }
}

/// Normalize a JSON object classified by a JSON pack.
pub fn normalize_json_object(
    obj: &Map<String, Value>,
    pack_id: &str,
    timestamp_field: Option<&str>,
    level_field: Option<&str>,
    body_field: Option<&str>,
    level_map: &Map<String, Value>,
) -> Map<String, Value> {
    let mut map = obj.clone();
    let root = Value::Object(obj.clone());

    if let Some(lf) = level_field {
        if let Some(raw) = json_path(&root, lf) {
            if let Some(level) = map_level(raw, level_map) {
                map.insert("level".into(), json!(level));
            }
        }
    }
    if let Some(bf) = body_field {
        if let Some(msg) = json_path(&root, bf) {
            if !map.contains_key("msg") {
                let text = match msg {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                map.insert("msg".into(), json!(text));
            }
        }
    }
    if let Some(tf) = timestamp_field {
        if let Some(t) = json_path(&root, tf) {
            if !map.contains_key("@timestamp") && !map.contains_key("timestamp") {
                map.insert("@timestamp".into(), t.clone());
            }
        }
    }

    let stable = mizpah_format_id(pack_id);
    map.insert("_format".into(), json!(stable));
    map.insert("_pack_format".into(), json!(pack_id));
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mizpah_format_id_and_primary_packs() {
        assert_eq!(mizpah_format_id("syslog_log"), "syslog");
        assert_eq!(mizpah_format_id("bunyan_log"), "bunyan");
        assert_eq!(mizpah_format_id("pino_log"), "pino");
        assert_eq!(mizpah_format_id("journald_json_log"), "journald");
        assert_eq!(mizpah_format_id("access_log"), "access_log");
        assert_eq!(mizpah_format_id("slog_json_log"), "slog");
        assert_eq!(mizpah_format_id("zerolog_json_log"), "zerolog");
        assert_eq!(mizpah_format_id("logrus_json_log"), "logrus");
        assert_eq!(mizpah_format_id("structlog_json_log"), "structlog");
        assert_eq!(mizpah_format_id("custom_x"), "custom_x");
        assert!(is_mizpah_primary_pack("syslog_log"));
        assert!(is_mizpah_primary_pack("slog_json_log"));
        assert!(!is_mizpah_primary_pack("ecs_log"));
    }

    #[test]
    fn json_path_exact_nested_and_empty() {
        let root = json!({"a":{"b":1}, "log.level":"error", "@timestamp":"t"});
        assert_eq!(json_path(&root, "").unwrap(), &root);
        assert_eq!(
            json_path(&root, "log.level").and_then(|v| v.as_str()),
            Some("error")
        );
        assert_eq!(json_path(&root, "a/b").and_then(|v| v.as_i64()), Some(1));
        assert_eq!(json_path(&root, "a.b").and_then(|v| v.as_i64()), Some(1));
        assert!(json_path(&root, "missing/x").is_none());
    }

    #[test]
    fn normalize_level_str_variants() {
        assert_eq!(normalize_level_str("WARNING"), "warn");
        assert_eq!(normalize_level_str("err"), "error");
        assert_eq!(normalize_level_str("critical"), "critical");
        assert_eq!(normalize_level_str("crit"), "critical");
        assert_eq!(normalize_level_str("fatal"), "fatal");
        assert_eq!(normalize_level_str("emergency"), "error");
        assert_eq!(normalize_level_str("alert"), "error");
        assert_eq!(normalize_level_str("information"), "info");
        assert_eq!(normalize_level_str("debug"), "debug");
    }

    #[test]
    fn map_level_numeric_string_and_regex() {
        let mut level_map = Map::new();
        level_map.insert("info".into(), json!(30));
        level_map.insert("error".into(), json!("50"));
        level_map.insert("warn".into(), json!("^W"));
        assert_eq!(map_level(&json!(30), &level_map).as_deref(), Some("info"));
        assert_eq!(
            map_level(&json!(50u64), &level_map).as_deref(),
            Some("error")
        );
        assert_eq!(map_level(&json!(99), &level_map).as_deref(), Some("99"));
        assert_eq!(
            map_level(&json!("WARN"), &level_map).as_deref(),
            Some("warn")
        );
        assert_eq!(
            map_level(&json!("info"), &level_map).as_deref(),
            Some("info")
        );
        assert_eq!(map_level(&json!(true), &level_map).as_deref(), Some("true"));
        let mut loose = Map::new();
        loose.insert("error".into(), json!("ERROR"));
        assert_eq!(map_level(&json!("error"), &loose).as_deref(), Some("error"));
    }

    #[test]
    fn apply_text_aliases_msg_timestamp_request_status() {
        let mut map = Map::new();
        map.insert("body".into(), json!("hello"));
        map.insert("ts".into(), json!("t0"));
        map.insert("request".into(), json!("GET /x HTTP/1.1"));
        map.insert("status".into(), json!("500"));
        apply_text_aliases(&mut map);
        assert_eq!(map.get("msg").and_then(|v| v.as_str()), Some("hello"));
        assert_eq!(map.get("@timestamp").and_then(|v| v.as_str()), Some("t0"));
        assert_eq!(map.get("method").and_then(|v| v.as_str()), Some("GET"));
        assert_eq!(map.get("path").and_then(|v| v.as_str()), Some("/x"));
        assert_eq!(map.get("level").and_then(|v| v.as_str()), Some("error"));

        let mut map2 = Map::new();
        map2.insert("message".into(), json!("m"));
        map2.insert("timestamp".into(), json!("t1"));
        map2.insert("status".into(), json!(404));
        apply_text_aliases(&mut map2);
        assert_eq!(map2.get("msg").and_then(|v| v.as_str()), Some("m"));
        assert_eq!(map2.get("@timestamp").and_then(|v| v.as_str()), Some("t1"));
        assert_eq!(map2.get("level").and_then(|v| v.as_str()), Some("warn"));

        let mut map3 = Map::new();
        map3.insert("status".into(), json!(200));
        apply_text_aliases(&mut map3);
        assert_eq!(map3.get("level").and_then(|v| v.as_str()), Some("info"));
    }

    #[test]
    fn normalize_json_object_applies_fields() {
        let mut obj = Map::new();
        obj.insert("log".into(), json!({"level":"error"}));
        obj.insert("message".into(), json!({"text":"boom"}));
        obj.insert("time".into(), json!("2020-01-01T00:00:00Z"));
        let level_map = Map::new();
        let out = normalize_json_object(
            &obj,
            "syslog_log",
            Some("time"),
            Some("log/level"),
            Some("message/text"),
            &level_map,
        );
        assert_eq!(out.get("level").and_then(|v| v.as_str()), Some("error"));
        assert_eq!(out.get("msg").and_then(|v| v.as_str()), Some("boom"));
        assert_eq!(
            out.get("@timestamp").and_then(|v| v.as_str()),
            Some("2020-01-01T00:00:00Z")
        );
        assert_eq!(out.get("_format").and_then(|v| v.as_str()), Some("syslog"));
        assert_eq!(
            out.get("_pack_format").and_then(|v| v.as_str()),
            Some("syslog_log")
        );
    }

    #[test]
    fn json_path_skips_empty_path_segments() {
        let root = json!({"a": {"b": 1}});
        assert_eq!(json_path(&root, "a//b").and_then(|v| v.as_i64()), Some(1));
    }

    #[test]
    fn normalize_level_str_fatal_and_alert_branches() {
        assert_eq!(normalize_level_str("FATAL"), "fatal");
        assert_eq!(normalize_level_str("fatal"), "fatal");
        assert_eq!(normalize_level_str("emergency"), "error");
        assert_eq!(normalize_level_str("alert"), "error");
        assert_eq!(normalize_level_str("crit"), "critical");
    }

    #[test]
    fn map_level_bool_and_bad_regex() {
        let mut level_map = Map::new();
        level_map.insert("warn".into(), json!("^["));
        assert_eq!(map_level(&json!(true), &level_map).as_deref(), Some("true"));
        assert_eq!(
            map_level(&json!("WARN"), &level_map).as_deref(),
            Some("warn")
        );
    }

    #[test]
    fn apply_text_aliases_timestamp_variants_and_existing_timestamp() {
        let mut map = Map::new();
        map.insert("time".into(), json!("t-time"));
        apply_text_aliases(&mut map);
        assert_eq!(
            map.get("@timestamp").and_then(|v| v.as_str()),
            Some("t-time")
        );

        let mut map2 = Map::new();
        map2.insert("timestamp".into(), json!("t-only"));
        apply_text_aliases(&mut map2);
        assert_eq!(
            map2.get("@timestamp").and_then(|v| v.as_str()),
            Some("t-only")
        );

        let mut map3 = Map::new();
        map3.insert("request".into(), json!("GET"));
        map3.insert("status".into(), json!("404"));
        apply_text_aliases(&mut map3);
        assert_eq!(map3.get("method").and_then(|v| v.as_str()), Some("GET"));
        assert_eq!(map3.get("level").and_then(|v| v.as_str()), Some("warn"));
    }

    #[test]
    fn normalize_json_object_non_string_body() {
        let mut obj = Map::new();
        obj.insert("message".into(), json!({"code": 42}));
        let out = normalize_json_object(&obj, "pino_log", None, None, Some("message"), &Map::new());
        assert_eq!(
            out.get("msg").and_then(|v| v.as_str()),
            Some("{\"code\":42}")
        );
    }
}
