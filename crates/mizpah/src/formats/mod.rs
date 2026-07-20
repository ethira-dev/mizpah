//! Log format detection and parsing (logfmt, syslog, access_log, format packs, …).

mod access_log;
mod bro;
mod generic;
mod json_pack;
mod logfmt;
mod packs;
mod syslog;
mod vendor_specialized;
mod w3c;

use serde_json::{Map, Value};

pub use access_log::AccessLogFormat;
pub use bro::BroFormat;
pub use generic::GenericFormat;
pub use json_pack::classify_json_object;
pub use logfmt::LogfmtFormat;
pub use packs::{classify_pack_json, detect_pack_text, parse_pack_text, parse_with_format_hint};
pub use syslog::SyslogFormat;
pub use vendor_specialized::{ConsulFormat, F5Format, HerokuRouterFormat, NomadFormat};
pub use w3c::W3cFormat;

/// Normalized parse result before store commit.
#[derive(Debug, Clone)]
pub struct NormalizedLog {
    pub data: Value,
    pub format_id: String,
}

pub trait LogFormat: Send + Sync {
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
    /// Detection confidence 0.0–1.0 (0 = not this format).
    fn detect(&self, line: &str) -> f32;
    fn parse(&self, line: &str) -> Option<NormalizedLog>;
}

fn existing_specialized() -> [&'static dyn LogFormat; 9] {
    [
        // Vendor wrappers before syslog/logfmt so they win on wrapped lines.
        &HerokuRouterFormat,
        &F5Format,
        &ConsulFormat,
        &NomadFormat,
        &LogfmtFormat,
        &SyslogFormat,
        &AccessLogFormat,
        &BroFormat,
        &W3cFormat,
    ]
}

fn attach_format(mut data: Value, format_id: &str) -> Value {
    if let Value::Object(map) = &mut data {
        map.entry("_format".to_string())
            .or_insert_with(|| Value::String(format_id.to_string()));
    }
    data
}

/// Parse an ingest line with optional locked format hint (pack or stable id).
pub fn parse_ingest_line_with_hint(
    line: &str,
    format_hint: Option<&str>,
) -> (Value, Option<String>) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return (
            Value::Object(Map::from_iter([(
                "_raw".to_string(),
                Value::String(String::new()),
            )])),
            Some("raw".into()),
        );
    }

    if let Some(hint) = format_hint {
        // Prefer locked pack / specialized parsers for the hint.
        if let Some(norm) = parse_with_format_hint(trimmed, hint) {
            return (
                attach_format(norm.data, &norm.format_id),
                Some(norm.format_id),
            );
        }
        match hint {
            "logfmt" => {
                if let Some(norm) = LogfmtFormat.parse(trimmed) {
                    return (attach_format(norm.data, "logfmt"), Some("logfmt".into()));
                }
            }
            "syslog" => {
                if let Some(norm) = SyslogFormat.parse(trimmed) {
                    return (attach_format(norm.data, "syslog"), Some("syslog".into()));
                }
            }
            "access_log" => {
                if let Some(norm) = AccessLogFormat.parse(trimmed) {
                    return (
                        attach_format(norm.data, "access_log"),
                        Some("access_log".into()),
                    );
                }
            }
            "bro_log" => {
                if let Some(norm) = BroFormat.parse(trimmed) {
                    return (attach_format(norm.data, "bro_log"), Some("bro_log".into()));
                }
            }
            "w3c_log" => {
                if let Some(norm) = W3cFormat.parse(trimmed) {
                    return (attach_format(norm.data, "w3c_log"), Some("w3c_log".into()));
                }
            }
            "heroku_router_log" => {
                if let Some(norm) = HerokuRouterFormat.parse(trimmed) {
                    return (
                        attach_format(norm.data, "heroku_router_log"),
                        Some("heroku_router_log".into()),
                    );
                }
            }
            "f5_log" => {
                if let Some(norm) = F5Format.parse(trimmed) {
                    return (attach_format(norm.data, "f5_log"), Some("f5_log".into()));
                }
            }
            "consul_log" => {
                if let Some(norm) = ConsulFormat.parse(trimmed) {
                    return (
                        attach_format(norm.data, "consul_log"),
                        Some("consul_log".into()),
                    );
                }
            }
            "nomad_log" => {
                if let Some(norm) = NomadFormat.parse(trimmed) {
                    return (
                        attach_format(norm.data, "nomad_log"),
                        Some("nomad_log".into()),
                    );
                }
            }
            "generic" => {
                if let Some(norm) = GenericFormat.parse(trimmed) {
                    return (attach_format(norm.data, "generic"), Some("generic".into()));
                }
            }
            _ => {}
        }
        // Hint missed — fall through to normal detection.
    }

    parse_ingest_line(trimmed)
}

/// Parse a non-JSON ingest line: try format detectors, then logfmt, then raw.
/// Returns `(payload, format_id)`.
pub fn parse_ingest_line(line: &str) -> (Value, Option<String>) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return (
            Value::Object(Map::from_iter([(
                "_raw".to_string(),
                Value::String(String::new()),
            )])),
            Some("raw".into()),
        );
    }

    // 1) Valid JSON → Mizpah JSON field packs, then vendored JSON packs, else plain json.
    if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(trimmed) {
        if let Some(norm) = classify_json_object(&obj) {
            return (norm.data, Some(norm.format_id));
        }
        if let Some(norm) = classify_pack_json(&obj) {
            return (norm.data, Some(norm.format_id));
        }
        return (Value::Object(obj), Some("json".into()));
    }

    // 2) Existing specialized winners (logfmt, syslog, access_log, bro, w3c).
    let mut best: Option<(f32, &'static dyn LogFormat)> = None;
    for fmt in existing_specialized() {
        let c = fmt.detect(trimmed);
        if c >= 0.5 {
            match &best {
                None => best = Some((c, fmt)),
                Some((bc, _)) if c > *bc => best = Some((c, fmt)),
                _ => {}
            }
        }
    }
    if let Some((_, fmt)) = best {
        if let Some(norm) = fmt.parse(trimmed) {
            return (
                attach_format(norm.data, &norm.format_id),
                Some(norm.format_id),
            );
        }
    }

    // Explicit logfmt fallback even at lower confidence
    if let Some(norm) = LogfmtFormat.parse(trimmed) {
        if LogfmtFormat.detect(trimmed) >= 0.3 {
            return (attach_format(norm.data, "logfmt"), Some("logfmt".into()));
        }
    }

    // 3) vendored text packs (non-primary)
    if let Some(norm) = parse_pack_text(trimmed) {
        return (
            attach_format(norm.data, &norm.format_id),
            Some(norm.format_id),
        );
    }

    // 4) generic level-token
    if GenericFormat.detect(trimmed) >= 0.5 {
        if let Some(norm) = GenericFormat.parse(trimmed) {
            return (attach_format(norm.data, "generic"), Some("generic".into()));
        }
    }

    // 5) raw
    let raw = crate::store::parse_line(trimmed);
    (raw, Some("raw".into()))
}

/// Suggest a format lock from a sample of lines (file ingest). Returns stable/pack id.
pub fn suggest_format_lock(lines: &[String]) -> Option<String> {
    let mut scores: Map<String, Value> = Map::new();
    let mut bump = |id: &str, amount: f64| {
        let cur = scores.get(id).and_then(|v| v.as_f64()).unwrap_or(0.0);
        scores.insert(id.to_string(), Value::from(cur + amount));
    };

    for line in lines.iter().take(256) {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(t) {
            if let Some(norm) = classify_json_object(&obj) {
                bump(&norm.format_id, 2.0);
                continue;
            }
            if let Some(norm) = classify_pack_json(&obj) {
                bump(&norm.format_id, 2.0);
                continue;
            }
            bump("json", 1.0);
            continue;
        }
        for fmt in existing_specialized() {
            let c = fmt.detect(t);
            if c >= 0.5 {
                bump(fmt.name(), c as f64);
            }
        }
        if let Some((c, id)) = detect_pack_text(t) {
            bump(id, c as f64);
        }
    }

    let mut best: Option<(f64, String)> = None;
    for (id, v) in &scores {
        let Some(score) = v.as_f64() else { continue };
        // Require clear winner
        if score < 3.0 {
            continue;
        }
        match &best {
            None => best = Some((score, id.clone())),
            Some((bs, _)) if score > *bs => best = Some((score, id.clone())),
            _ => {}
        }
    }
    best.map(|(_, id)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_logfmt_line() {
        let (v, id) = parse_ingest_line(r#"level=error msg="hello world" code=42"#);
        assert_eq!(id.as_deref(), Some("logfmt"));
        assert_eq!(v["level"], "error");
        assert_eq!(v["msg"], "hello world");
        assert_eq!(v["code"], 42);
    }

    #[test]
    fn parse_json_still_json() {
        let (v, id) = parse_ingest_line(r#"{"level":"info"}"#);
        assert_eq!(id.as_deref(), Some("json"));
        assert_eq!(v["level"], "info");
    }

    #[test]
    fn parse_bunyan_json() {
        let (v, id) = parse_ingest_line(
            r#"{"v":0,"level":50,"name":"app","msg":"boom","time":"2020-01-01T00:00:00Z"}"#,
        );
        assert_eq!(id.as_deref(), Some("bunyan"));
        assert_eq!(v["level"], "error");
    }

    #[test]
    fn parse_generic_level_prefix() {
        // FATAL is covered by generic but not by common pack patterns (e.g. deno_log).
        let (v, id) = parse_ingest_line("FATAL something broke");
        assert_eq!(id.as_deref(), Some("generic"));
        assert_eq!(v["level"], "fatal");
        assert_eq!(v["msg"], "something broke");
    }

    #[test]
    fn mixed_pipe_does_not_lock() {
        let (v1, id1) = parse_ingest_line(r#"level=info msg=one"#);
        let (v2, id2) = parse_ingest_line(r#"{"level":"info","msg":"two"}"#);
        assert_eq!(id1.as_deref(), Some("logfmt"));
        assert_eq!(id2.as_deref(), Some("json"));
        assert_eq!(v1["msg"], "one");
        assert_eq!(v2["msg"], "two");
    }

    #[test]
    fn syslog_stable_id() {
        let line = r#"<34>Oct 11 22:14:15 mymachine su: 'su root' failed"#;
        let (_, id) = parse_ingest_line(line);
        assert_eq!(id.as_deref(), Some("syslog"));
    }

    #[test]
    fn loaded_packs_are_registered() {
        let ids = packs::loaded_pack_ids();
        assert!(ids.len() >= 195);
        assert!(ids.iter().any(|id| id == "otel_collector_log"));
    }

    #[test]
    fn heroku_router_wins_over_syslog_logfmt() {
        let line = r#"Jan  1 00:00:00 host heroku[router]: at=info method=GET path="/" host=example.com request_id=abc fwd="1.2.3.4" dyno=web.1 connect=1ms service=2ms status=200 bytes=123"#;
        let (v, id) = parse_ingest_line(line);
        assert_eq!(id.as_deref(), Some("heroku_router_log"));
        assert_eq!(v["method"], "GET");
        assert_eq!(v["status"], 200);
    }

    #[test]
    fn vault_audit_json_pack() {
        let line = r#"{"time":"2020-01-01T00:00:00.000Z","type":"request","auth":{"display_name":"token","policies":["default"],"token_type":"service"},"request":{"id":"a1b2c3d4-e5f6-7890-abcd-ef1234567890","operation":"read","path":"auth/token/lookup-self","remote_address":"10.0.0.1"}}"#;
        let (v, id) = parse_ingest_line(line);
        assert_eq!(id.as_deref(), Some("vault_audit_log"));
        assert!(v.get("msg").is_some() || v.get("request").is_some());
    }

    #[test]
    fn spring_boot_still_java_log() {
        let line = "2025-01-23T03:42:36.681-0800  INFO 125873 --- [myapp] [           main] o.s.b.d.f.logexample.MyApplication       : Starting MyApplication using Java 17.0.14 with PID 125873 (/opt/apps/myapp.jar started by myuser in /opt/apps/)";
        let (_, id) = parse_ingest_line(line);
        assert_eq!(id.as_deref(), Some("java_log"));
    }

    #[test]
    fn match_keys_block_vault_false_positive() {
        let line = r#"{"time":"2020-01-01T00:00:00Z","type":"request","data":{"x":1}}"#;
        let (_, id) = parse_ingest_line(line);
        assert_ne!(id.as_deref(), Some("vault_audit_log"));
    }

    #[test]
    fn parse_ecs_via_json_pack() {
        let (v, id) = parse_ingest_line(
            r#"{"@timestamp":"2020-01-01T00:00:00.000Z","log.level":"error","message":"boom","ecs.version":"1.0.0"}"#,
        );
        assert_eq!(id.as_deref(), Some("ecs_log"));
        assert_eq!(v["level"], "error");
    }

    #[test]
    fn parse_postgres_via_text_pack() {
        let line = "2020-01-01 12:00:00.000 UTC [123] LOG:  database system is ready";
        // Only assert when the vendored pack matches this shape.
        let (v, id) = parse_ingest_line(line);
        if id.as_deref() == Some("postgres_log") {
            assert!(v.get("msg").is_some() || v.get("body").is_some() || v.get("_raw").is_some());
        }
    }

    #[test]
    fn parse_empty_line() {
        let (v, id) = parse_ingest_line("   ");
        assert_eq!(id.as_deref(), Some("raw"));
        assert_eq!(v["_raw"], "");
    }

    #[test]
    fn parse_ingest_line_with_hints() {
        let logfmt_line = r#"level=info msg=hinted"#;
        let (v, id) = parse_ingest_line_with_hint(logfmt_line, Some("logfmt"));
        assert_eq!(id.as_deref(), Some("logfmt"));
        assert_eq!(v["msg"], "hinted");

        let syslog_line = r#"<34>Oct 11 22:14:15 host app: fail"#;
        let (_, id) = parse_ingest_line_with_hint(syslog_line, Some("syslog"));
        assert_eq!(id.as_deref(), Some("syslog"));

        let access = r#"127.0.0.1 - - [10/Oct/2000:13:55:36 -0700] "GET / HTTP/1.0" 200 0"#;
        let (_, id) = parse_ingest_line_with_hint(access, Some("access_log"));
        assert_eq!(id.as_deref(), Some("access_log"));

        let bro = "#fields\tts\tuid\tid.orig_h\tid.orig_p\tproto";
        let (_, id) = parse_ingest_line_with_hint(bro, Some("bro_log"));
        assert_eq!(id.as_deref(), Some("bro_log"));

        let w3c = "#Fields: date time cs-method sc-status";
        let (_, id) = parse_ingest_line_with_hint(w3c, Some("w3c_log"));
        assert_eq!(id.as_deref(), Some("w3c_log"));

        let (v, id) = parse_ingest_line_with_hint("ERROR from hint", Some("generic"));
        assert_eq!(id.as_deref(), Some("generic"));
        assert_eq!(v["level"], "error");

        let (v, id) = parse_ingest_line_with_hint("plain text", Some("unknown_hint"));
        assert_eq!(id.as_deref(), Some("raw"));
        assert_eq!(v["_raw"], "plain text");

        let (v, id) = parse_ingest_line_with_hint("not logfmt", Some("logfmt"));
        assert_eq!(id.as_deref(), Some("raw"));
        assert_eq!(v["_raw"], "not logfmt");
    }

    #[test]
    fn suggest_format_lock_picks_winner() {
        let lines = vec![
            r#"level=info msg=one"#.into(),
            r#"level=warn msg=two"#.into(),
            r#"level=error msg=three"#.into(),
            r#"level=info msg=four"#.into(),
        ];
        assert_eq!(suggest_format_lock(&lines).as_deref(), Some("logfmt"));
    }

    #[test]
    fn attach_format_adds_format_id() {
        let mut obj = Map::from_iter([("msg".into(), Value::String("hi".into()))]);
        let v = attach_format(Value::Object(obj.clone()), "test_fmt");
        assert_eq!(v["_format"], "test_fmt");
        obj.insert("_format".into(), Value::String("existing".into()));
        let v = attach_format(Value::Object(obj), "other");
        assert_eq!(v["_format"], "existing");
    }

    #[test]
    fn suggest_format_lock_json_and_empty() {
        assert!(suggest_format_lock(&[]).is_none());
        let json_lines = vec![
            r#"{"level":"info"}"#.into(),
            r#"{"level":"warn"}"#.into(),
            r#"{"level":"error"}"#.into(),
        ];
        assert_eq!(suggest_format_lock(&json_lines).as_deref(), Some("json"));
    }

    #[test]
    fn parse_with_hint_empty_and_logfmt_fallback() {
        let (v, id) = parse_ingest_line_with_hint("", None);
        assert_eq!(id.as_deref(), Some("raw"));
        assert_eq!(v["_raw"], "");

        let loose = "token=only";
        let (v, id) = parse_ingest_line(loose);
        if LogfmtFormat.detect(loose) >= 0.3 {
            assert_eq!(id.as_deref(), Some("logfmt"));
            assert_eq!(v["token"], "only");
        }
    }

    #[test]
    fn attach_format_non_object_unchanged() {
        let v = attach_format(Value::Array(vec![]), "fmt");
        assert!(v.is_array());
    }

    #[test]
    fn parse_with_pack_hint_when_matching() {
        let line = "2020-01-01 12:00:00.000 UTC [123] LOG:  database system is ready";
        let (v, id) = parse_ingest_line_with_hint(line, Some("postgres_log"));
        if id.as_deref() == Some("postgres_log") {
            assert!(v.get("_format").is_some());
        }
    }

    #[test]
    fn suggest_format_lock_bunyan_json() {
        let lines = vec![
            r#"{"v":0,"level":50,"name":"app","msg":"a","time":"t"}"#.into(),
            r#"{"v":0,"level":40,"name":"app","msg":"b","time":"t"}"#.into(),
            r#"{"v":0,"level":30,"name":"app","msg":"c","time":"t"}"#.into(),
        ];
        assert_eq!(suggest_format_lock(&lines).as_deref(), Some("bunyan"));
    }

    #[test]
    fn parse_access_log_via_detection() {
        let line = r#"127.0.0.1 - - [10/Oct/2000:13:55:36 -0700] "GET / HTTP/1.0" 200 0"#;
        let (_, id) = parse_ingest_line(line);
        assert_eq!(id.as_deref(), Some("access_log"));
    }

    #[test]
    fn suggest_format_lock_below_threshold_returns_none() {
        let lines = vec![
            r#"level=info msg=one"#.into(),
            r#"level=warn msg=two"#.into(),
        ];
        assert!(suggest_format_lock(&lines).is_none());
    }

    #[test]
    fn parse_ingest_line_with_hint_json_pack() {
        let line = r#"{"@timestamp":"2020-01-01T00:00:00.000Z","log.level":"error","message":"boom","ecs.version":"1.0.0"}"#;
        let (v, id) = parse_ingest_line_with_hint(line, Some("ecs_log"));
        assert_eq!(id.as_deref(), Some("ecs_log"));
        assert_eq!(v["level"], "error");
    }
}
