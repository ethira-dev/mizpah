//! JSON field-map formats: journald, Bunyan, Pino, OTel, slog, zerolog, logrus,
//! structlog, plus user packs from formats_dir.

use super::{LogFormat, NormalizedLog};
use crate::config::MizpahConfig;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::fs;
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonFieldPack {
    pub id: String,
    /// Keys that must be present for detection.
    #[serde(default)]
    pub match_keys: Vec<String>,
    /// Optional key whose presence boosts confidence.
    #[serde(default)]
    pub match_any: Vec<String>,
    #[serde(default)]
    pub level_field: Option<String>,
    #[serde(default)]
    pub msg_field: Option<String>,
    #[serde(default)]
    pub time_field: Option<String>,
    /// Numeric Bunyan/Pino style level map (string keys → names).
    #[serde(default)]
    pub level_map: Map<String, Value>,
}

fn builtins() -> &'static [JsonFieldPack] {
    static PACKS: OnceLock<Vec<JsonFieldPack>> = OnceLock::new();
    PACKS.get_or_init(|| {
        vec![
            JsonFieldPack {
                id: "bunyan".into(),
                match_keys: vec!["v".into(), "msg".into(), "level".into()],
                match_any: vec!["time".into(), "name".into()],
                level_field: Some("level".into()),
                msg_field: Some("msg".into()),
                time_field: Some("time".into()),
                level_map: Map::from_iter([
                    ("10".into(), json!("trace")),
                    ("20".into(), json!("debug")),
                    ("30".into(), json!("info")),
                    ("40".into(), json!("warn")),
                    ("50".into(), json!("error")),
                    ("60".into(), json!("fatal")),
                ]),
            },
            JsonFieldPack {
                id: "pino".into(),
                match_keys: vec!["level".into(), "time".into()],
                match_any: vec!["msg".into(), "pid".into(), "hostname".into()],
                level_field: Some("level".into()),
                msg_field: Some("msg".into()),
                time_field: Some("time".into()),
                level_map: Map::from_iter([
                    ("10".into(), json!("trace")),
                    ("20".into(), json!("debug")),
                    ("30".into(), json!("info")),
                    ("40".into(), json!("warn")),
                    ("50".into(), json!("error")),
                    ("60".into(), json!("fatal")),
                ]),
            },
            JsonFieldPack {
                id: "otel".into(),
                match_keys: vec![],
                match_any: vec![
                    "severityText".into(),
                    "severityNumber".into(),
                    "traceId".into(),
                    "body".into(),
                ],
                level_field: Some("severityText".into()),
                msg_field: Some("body".into()),
                time_field: Some("timestamp".into()),
                level_map: Map::new(),
            },
            JsonFieldPack {
                id: "journald".into(),
                match_keys: vec!["MESSAGE".into()],
                match_any: vec![
                    "PRIORITY".into(),
                    "__REALTIME_TIMESTAMP".into(),
                    "_SYSTEMD_UNIT".into(),
                ],
                level_field: Some("PRIORITY".into()),
                msg_field: Some("MESSAGE".into()),
                time_field: Some("__REALTIME_TIMESTAMP".into()),
                level_map: Map::from_iter([
                    ("0".into(), json!("fatal")),
                    ("1".into(), json!("fatal")),
                    ("2".into(), json!("critical")),
                    ("3".into(), json!("error")),
                    ("4".into(), json!("warn")),
                    ("5".into(), json!("info")),
                    ("6".into(), json!("info")),
                    ("7".into(), json!("debug")),
                ]),
            },
            // Go slog JSONHandler — string levels (INFO/DEBUG); avoid stealing pino.
            JsonFieldPack {
                id: "slog".into(),
                match_keys: vec!["time".into(), "level".into(), "msg".into()],
                match_any: vec![],
                level_field: Some("level".into()),
                msg_field: Some("msg".into()),
                time_field: Some("time".into()),
                level_map: Map::new(),
            },
            // zerolog — distinctive `message` key (not `msg`).
            JsonFieldPack {
                id: "zerolog".into(),
                match_keys: vec!["level".into(), "message".into()],
                match_any: vec!["time".into()],
                level_field: Some("level".into()),
                msg_field: Some("message".into()),
                time_field: Some("time".into()),
                level_map: Map::new(),
            },
            // logrus JSONFormatter — lowercase string levels.
            JsonFieldPack {
                id: "logrus".into(),
                match_keys: vec!["time".into(), "level".into(), "msg".into()],
                match_any: vec![],
                level_field: Some("level".into()),
                msg_field: Some("msg".into()),
                time_field: Some("time".into()),
                level_map: Map::new(),
            },
            // structlog JSON — `event` is the message field.
            JsonFieldPack {
                id: "structlog".into(),
                match_keys: vec!["event".into(), "level".into()],
                match_any: vec!["timestamp".into(), "logger".into()],
                level_field: Some("level".into()),
                msg_field: Some("event".into()),
                time_field: Some("timestamp".into()),
                level_map: Map::new(),
            },
        ]
    })
}

fn load_user_packs_from_dir(dir: &std::path::Path) -> Vec<JsonFieldPack> {
    if !dir.is_dir() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for ent in rd.flatten() {
            let path = ent.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            match fs::read_to_string(&path) {
                Ok(text) => match toml::from_str::<JsonFieldPack>(&text) {
                    Ok(pack) if !pack.id.is_empty() => out.push(pack),
                    Err(e) => {
                        tracing::warn!(error = %e, path = %path.display(), "bad format pack");
                    }
                    _ => {}
                },
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(), "read format pack failed");
                }
            }
        }
    }
    out
}

fn load_user_packs() -> Vec<JsonFieldPack> {
    let Ok(dir) = MizpahConfig::formats_dir() else {
        return Vec::new();
    };
    load_user_packs_from_dir(&dir)
}

fn pack_confidence(obj: &Map<String, Value>, pack: &JsonFieldPack) -> f32 {
    if pack.match_keys.is_empty() && pack.match_any.is_empty() {
        return 0.0;
    }
    if !pack.match_keys.is_empty() && !pack.match_keys.iter().all(|k| obj.contains_key(k)) {
        return 0.0;
    }
    let any_hits = pack
        .match_any
        .iter()
        .filter(|k| obj.contains_key(k.as_str()))
        .count();
    // OTel / match_any-only packs: need at least two signals.
    if pack.match_keys.is_empty() {
        if any_hits < 2 {
            return 0.0;
        }
        let score = 0.55 + 0.1 * (any_hits as f32 / pack.match_any.len().max(1) as f32);
        return score.clamp(0.0, 0.99);
    }
    let mut score = 0.55;
    if !pack.match_any.is_empty() {
        score += 0.1 * (any_hits as f32 / pack.match_any.len() as f32);
    }
    // Bunyan: require v == 0
    if pack.id == "bunyan" {
        match obj.get("v") {
            Some(Value::Number(n)) if n.as_u64() == Some(0) => score += 0.15,
            Some(Value::Number(_)) => return 0.0,
            _ => {}
        }
    }
    // Pino often lacks `v`; prefer when no bunyan v
    if pack.id == "pino" && obj.contains_key("v") {
        score -= 0.2;
    }
    // Pino uses numeric levels; string levels belong to slog/logrus.
    if pack.id == "pino" {
        if let Some(Value::String(_)) = obj.get("level") {
            score -= 0.35;
        }
    }
    // slog: UPPERCASE string levels; reject numeric (pino) / lowercase (logrus).
    if pack.id == "slog" {
        match obj.get("level") {
            Some(Value::String(s)) if matches!(s.as_str(), "DEBUG" | "INFO" | "WARN" | "ERROR") => {
                score += 0.25;
            }
            _ => return 0.0,
        }
        if obj.contains_key("pid") {
            score -= 0.2;
        }
    }
    // logrus: lowercase string levels.
    if pack.id == "logrus" {
        match obj.get("level") {
            Some(Value::String(s)) => {
                let lower = s.to_ascii_lowercase();
                if matches!(
                    lower.as_str(),
                    "trace" | "debug" | "info" | "warning" | "warn" | "error" | "fatal" | "panic"
                ) && s.chars().all(|c| !c.is_ascii_uppercase())
                {
                    score += 0.25;
                } else {
                    return 0.0;
                }
            }
            _ => return 0.0,
        }
        if obj.contains_key("pid") {
            score -= 0.2;
        }
    }
    score.clamp(0.0, 0.99)
}

fn remap_level(pack: &JsonFieldPack, raw: &Value) -> Option<String> {
    let key = match raw {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        _ => return None,
    };
    if let Some(mapped) = pack.level_map.get(&key).and_then(|v| v.as_str()) {
        return Some(mapped.to_string());
    }
    let lower = key.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "trace" | "debug" | "info" | "warn" | "warning" | "error" | "fatal" | "critical"
    ) {
        return Some(if lower == "warning" {
            "warn".into()
        } else {
            lower
        });
    }
    Some(key)
}

fn apply_pack(obj: &Map<String, Value>, pack: &JsonFieldPack) -> NormalizedLog {
    let mut map = obj.clone();
    if let Some(lf) = &pack.level_field {
        if let Some(raw) = obj.get(lf) {
            if let Some(level) = remap_level(pack, raw) {
                map.insert("level".into(), json!(level));
            }
        }
    }
    if let Some(mf) = &pack.msg_field {
        if let Some(msg) = obj.get(mf) {
            if !map.contains_key("msg") {
                let text = match msg {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                map.insert("msg".into(), json!(text));
            }
        }
    }
    if let Some(tf) = &pack.time_field {
        if let Some(t) = obj.get(tf) {
            if !map.contains_key("@timestamp") && !map.contains_key("timestamp") {
                map.insert("@timestamp".into(), t.clone());
            }
        }
    }
    map.insert("_format".into(), json!(pack.id));
    NormalizedLog {
        data: Value::Object(map),
        format_id: pack.id.clone(),
    }
}

/// Try to classify a JSON object as a known JSON format pack.
pub fn classify_json_object(obj: &Map<String, Value>) -> Option<NormalizedLog> {
    let user = load_user_packs();
    let mut best: Option<(f32, JsonFieldPack)> = None;
    for pack in builtins().iter().chain(user.iter()) {
        let c = pack_confidence(obj, pack);
        if c >= 0.5 {
            match &best {
                None => best = Some((c, pack.clone())),
                Some((bc, _)) if c > *bc => best = Some((c, pack.clone())),
                _ => {}
            }
        }
    }
    best.map(|(_, pack)| apply_pack(obj, &pack))
}

/// Adapter so JSON packs participate in line-based detection (rarely used).
pub struct JsonPackFormat;

impl LogFormat for JsonPackFormat {
    fn name(&self) -> &'static str {
        "json_pack"
    }

    fn detect(&self, line: &str) -> f32 {
        let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(line.trim()) else {
            return 0.0;
        };
        builtins()
            .iter()
            .map(|p| pack_confidence(&obj, p))
            .fold(0.0_f32, f32::max)
    }

    fn parse(&self, line: &str) -> Option<NormalizedLog> {
        let Value::Object(obj) = serde_json::from_str(line.trim()).ok()? else {
            return None;
        };
        classify_json_object(&obj)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_bunyan() {
        let obj = json!({
            "v": 0,
            "level": 50,
            "name": "app",
            "msg": "boom",
            "time": "2020-01-01T00:00:00Z"
        });
        let Value::Object(map) = obj else { panic!() };
        let n = classify_json_object(&map).expect("bunyan");
        assert_eq!(n.format_id, "bunyan");
        assert_eq!(n.data["level"], "error");
        assert_eq!(n.data["msg"], "boom");
    }

    #[test]
    fn detects_pino() {
        let obj = json!({
            "level": 30,
            "time": 1577836800000u64,
            "pid": 1,
            "hostname": "host",
            "msg": "hello"
        });
        let Value::Object(map) = obj else { panic!() };
        let n = classify_json_object(&map).expect("pino");
        assert_eq!(n.format_id, "pino");
        assert_eq!(n.data["level"], "info");
    }

    #[test]
    fn detects_otel() {
        let obj = json!({
            "severityText": "ERROR",
            "body": "timeout",
            "traceId": "abc"
        });
        let Value::Object(map) = obj else { panic!() };
        let n = classify_json_object(&map).expect("otel");
        assert_eq!(n.format_id, "otel");
        assert_eq!(n.data["level"], "error");
        assert_eq!(n.data["msg"], "timeout");
    }

    #[test]
    fn detects_journald() {
        let obj = json!({
            "MESSAGE": "unit failed",
            "PRIORITY": "3",
            "__REALTIME_TIMESTAMP": "1577836800000000"
        });
        let Value::Object(map) = obj else { panic!() };
        let n = classify_json_object(&map).expect("journald");
        assert_eq!(n.format_id, "journald");
        assert_eq!(n.data["level"], "error");
        assert_eq!(n.data["msg"], "unit failed");
    }

    #[test]
    fn detects_slog_not_pino() {
        let obj = json!({
            "time": "2020-01-01T00:00:00Z",
            "level": "INFO",
            "msg": "hello"
        });
        let Value::Object(map) = obj else { panic!() };
        let n = classify_json_object(&map).expect("slog");
        assert_eq!(n.format_id, "slog");
        assert_eq!(n.data["level"], "info");
        assert_eq!(n.data["msg"], "hello");
    }

    #[test]
    fn detects_zerolog() {
        let obj = json!({
            "level": "info",
            "time": 1577836800u64,
            "message": "hello"
        });
        let Value::Object(map) = obj else { panic!() };
        let n = classify_json_object(&map).expect("zerolog");
        assert_eq!(n.format_id, "zerolog");
        assert_eq!(n.data["msg"], "hello");
    }

    #[test]
    fn detects_logrus_not_slog() {
        let obj = json!({
            "time": "2020-01-01T00:00:00Z",
            "level": "info",
            "msg": "hello"
        });
        let Value::Object(map) = obj else { panic!() };
        let n = classify_json_object(&map).expect("logrus");
        assert_eq!(n.format_id, "logrus");
        assert_eq!(n.data["level"], "info");
    }

    #[test]
    fn detects_structlog() {
        let obj = json!({
            "event": "request completed",
            "level": "info",
            "timestamp": "2020-01-01T00:00:00Z"
        });
        let Value::Object(map) = obj else { panic!() };
        let n = classify_json_object(&map).expect("structlog");
        assert_eq!(n.format_id, "structlog");
        assert_eq!(n.data["msg"], "request completed");
    }

    #[test]
    fn loads_user_format_packs_from_dir() {
        let dir = tempfile::tempdir().unwrap();
        let formats = dir.path().join("formats");
        std::fs::create_dir_all(&formats).unwrap();
        std::fs::write(
            formats.join("custom.toml"),
            r#"
id = "custom_pack"
matchKeys = ["customField"]
levelField = "lvl"
msgField = "text"
"#,
        )
        .unwrap();
        std::fs::write(formats.join("bad.toml"), "not valid {{{").unwrap();
        std::fs::write(formats.join("empty_id.toml"), r#"id = """#).unwrap();
        std::fs::write(formats.join("note.txt"), "skip").unwrap();

        let packs = super::load_user_packs_from_dir(&formats);
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].id, "custom_pack");

        assert!(super::load_user_packs_from_dir(&dir.path().join("missing")).is_empty());
        assert!(super::load_user_packs_from_dir(dir.path()).is_empty());
    }

    #[test]
    fn user_pack_classifies() {
        let dir = tempfile::tempdir().unwrap();
        let formats = dir.path().join("formats");
        std::fs::create_dir_all(&formats).unwrap();
        std::fs::write(
            formats.join("mine.toml"),
            r#"
id = "mine"
matchKeys = ["mineId"]
matchAny = ["extra"]
levelField = "lvl"
msgField = "text"
"#,
        )
        .unwrap();
        let _guard = crate::test_support::env_lock();
        std::env::set_var("MIZPAH_CONFIG_DIR", dir.path());
        // Avoid OTEL builtin keys (severityText/body) so the user pack wins.
        let obj = json!({
            "mineId": 1,
            "extra": true,
            "lvl": "WARNING",
            "text": "hello"
        });
        let Value::Object(map) = obj else { panic!() };
        let n = classify_json_object(&map).expect("user pack");
        assert_eq!(n.format_id, "mine");
        assert_eq!(n.data["level"], "warn");
        std::env::remove_var("MIZPAH_CONFIG_DIR");
    }

    #[test]
    fn pack_confidence_edge_cases() {
        let empty = JsonFieldPack {
            id: "empty".into(),
            match_keys: vec![],
            match_any: vec![],
            level_field: None,
            msg_field: None,
            time_field: None,
            level_map: Map::new(),
        };
        let map = Map::new();
        assert_eq!(pack_confidence(&map, &empty), 0.0);

        let bunyan_bad_v = json!({"v": 1, "level": 50, "msg": "x", "name": "a", "time": "t"});
        let Value::Object(bmap) = bunyan_bad_v else {
            panic!()
        };
        let bunyan = builtins().iter().find(|p| p.id == "bunyan").unwrap();
        assert_eq!(pack_confidence(&bmap, bunyan), 0.0);

        let otel_one = json!({"severityText": "INFO"});
        let Value::Object(omap) = otel_one else {
            panic!()
        };
        let otel = builtins().iter().find(|p| p.id == "otel").unwrap();
        assert_eq!(pack_confidence(&omap, otel), 0.0);
    }

    #[test]
    fn json_pack_format_adapter() {
        let fmt = JsonPackFormat;
        let line = r#"{"level":30,"time":1,"pid":1,"hostname":"h","msg":"hi"}"#;
        assert!(fmt.detect(line) >= 0.5);
        let n = fmt.parse(line).unwrap();
        assert_eq!(n.format_id, "pino");
        assert!(fmt.parse("not json").is_none());
    }

    #[test]
    fn apply_pack_preserves_existing_msg_and_timestamp() {
        let pack = JsonFieldPack {
            id: "test".into(),
            match_keys: vec!["k".into()],
            match_any: vec![],
            level_field: Some("lvl".into()),
            msg_field: Some("body".into()),
            time_field: Some("ts".into()),
            level_map: Map::new(),
        };
        let mut obj = Map::new();
        obj.insert("k".into(), json!(1));
        obj.insert("lvl".into(), json!("error"));
        obj.insert("msg".into(), json!("keep"));
        obj.insert("timestamp".into(), json!("existing"));
        obj.insert("ts".into(), json!("2020-01-01"));
        let n = apply_pack(&obj, &pack);
        assert_eq!(n.data["msg"], "keep");
        assert_eq!(n.data["timestamp"], "existing");
        assert_eq!(n.data["level"], "error");
    }

    #[test]
    fn load_user_packs_wrapper() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("MIZPAH_CONFIG_DIR", dir.path());
        assert!(load_user_packs().is_empty());
        std::env::remove_var("MIZPAH_CONFIG_DIR");
    }

    #[cfg(unix)]
    #[test]
    fn skips_unreadable_pack_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let formats = dir.path().join("formats");
        std::fs::create_dir_all(&formats).unwrap();
        let secret = formats.join("secret.toml");
        std::fs::write(&secret, r#"id = "secret""#).unwrap();
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert!(super::load_user_packs_from_dir(&formats).is_empty());
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn apply_pack_sets_msg_time_and_timestamp() {
        let pack = JsonFieldPack {
            id: "t".into(),
            match_keys: vec!["k".into()],
            match_any: vec![],
            level_field: None,
            msg_field: Some("body".into()),
            time_field: Some("ts".into()),
            level_map: Map::new(),
        };
        let mut obj = Map::new();
        obj.insert("k".into(), json!(1));
        obj.insert("body".into(), json!("hello"));
        obj.insert("ts".into(), json!("2024-01-01T00:00:00Z"));
        let n = apply_pack(&obj, &pack);
        assert_eq!(n.data["msg"], "hello");
        assert_eq!(n.data["@timestamp"], "2024-01-01T00:00:00Z");
    }

    #[test]
    fn remap_level_and_pino_penalty() {
        let pack = JsonFieldPack {
            id: "pino".into(),
            match_keys: vec!["level".into(), "time".into()],
            match_any: vec![],
            level_field: Some("level".into()),
            msg_field: None,
            time_field: None,
            level_map: Map::new(),
        };
        let with_v = json!({"level": 30, "time": 1, "v": 0});
        let Value::Object(vmap) = with_v else {
            panic!()
        };
        assert!(pack_confidence(&vmap, &pack) < 0.55);

        let pack = JsonFieldPack {
            id: "x".into(),
            match_keys: vec![],
            match_any: vec![],
            level_field: None,
            msg_field: None,
            time_field: None,
            level_map: Map::new(),
        };
        assert_eq!(remap_level(&pack, &json!("WARNING")), Some("warn".into()));
        assert_eq!(remap_level(&pack, &json!(true)), None);
    }
}
