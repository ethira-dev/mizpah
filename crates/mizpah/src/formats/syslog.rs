//! Minimal syslog (RFC3164-ish / RFC5424-ish) parser.

use super::{LogFormat, NormalizedLog};
use regex::Regex;
use serde_json::{json, Value};
use std::sync::OnceLock;

pub struct SyslogFormat;

fn rfc3164_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?x)
            ^
            (?:<(?P<pri>\d+)>)?
            (?P<ts>
                [A-Z][a-z]{2}\s+\d{1,2}\s+\d{2}:\d{2}:\d{2}
                |
                \d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?
            )
            \s+
            (?P<host>\S+)
            \s+
            (?P<app>[^:\s]+)(?::\s*|\s+)
            (?P<msg>.*)
            $",
        )
        .expect("syslog regex")
    })
}

impl LogFormat for SyslogFormat {
    fn name(&self) -> &'static str {
        "syslog"
    }

    fn detect(&self, line: &str) -> f32 {
        let t = line.trim();
        // Spring Boot console: `timestamp  LEVEL pid --- [thread] logger : msg`
        // ISO timestamps otherwise look like RFC3339 syslog and steal `java_log`.
        if t.contains(" --- [") {
            return 0.0;
        }
        if t.starts_with('<') && t.contains('>') {
            if rfc3164_re().is_match(t) {
                return 0.9;
            }
            return 0.4;
        }
        if rfc3164_re().is_match(t) {
            0.7
        } else {
            0.0
        }
    }

    fn parse(&self, line: &str) -> Option<NormalizedLog> {
        let t = line.trim();
        let caps = rfc3164_re().captures(t)?;
        let mut obj = json!({
            "timestamp": caps.name("ts").map_or("", |m| m.as_str()),
            "host": caps.name("host").map_or("", |m| m.as_str()),
            "app": caps.name("app").map_or("", |m| m.as_str()),
            "msg": caps.name("msg").map_or("", |m| m.as_str()),
            "_raw": t,
        });
        if let Some(pri) = caps.name("pri") {
            if let Ok(n) = pri.as_str().parse::<u32>() {
                let severity = n % 8;
                let level = match severity {
                    0..=3 => "error",
                    4 => "warn",
                    5..=6 => "info",
                    _ => "debug",
                };
                if let Value::Object(map) = &mut obj {
                    map.insert("priority".into(), json!(n));
                    map.insert("level".into(), json!(level));
                }
            }
        }
        Some(NormalizedLog {
            data: obj,
            format_id: "syslog".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pri_syslog() {
        let line = r#"<34>Oct 11 22:14:15 mymachine su: 'su root' failed"#;
        let fmt = SyslogFormat;
        assert!(fmt.detect(line) >= 0.5);
        let n = fmt.parse(line).unwrap();
        assert_eq!(n.data["host"], "mymachine");
        assert_eq!(n.data["level"], "error");
        assert_eq!(n.data["priority"], 34);
    }

    #[test]
    fn detect_branches() {
        let fmt = SyslogFormat;
        let plain = "Oct 11 22:14:15 host app: hello";
        assert!(fmt.detect(plain) >= 0.5);
        assert_eq!(fmt.detect("random text"), 0.0);
        assert_eq!(fmt.detect("<999>not-a-valid-syslog"), 0.4);
    }

    #[test]
    fn severity_and_rfc3339() {
        let fmt = SyslogFormat;
        assert_eq!(fmt.name(), "syslog");
        let warn = "<36>Oct 11 22:14:15 host app: warn msg";
        let info = "<30>Oct 11 22:14:15 host app: info msg";
        let debug = "<39>Oct 11 22:14:15 host app: debug msg";
        assert_eq!(fmt.parse(warn).unwrap().data["level"], "warn");
        assert_eq!(fmt.parse(info).unwrap().data["level"], "info");
        assert_eq!(fmt.parse(debug).unwrap().data["level"], "debug");

        let iso = "2024-01-15T12:00:00Z myhost nginx: started";
        let n = fmt.parse(iso).unwrap();
        assert_eq!(n.data["timestamp"], "2024-01-15T12:00:00Z");
        assert_eq!(n.data["app"], "nginx");

        let no_pri = "Oct 11 22:14:15 host app: no priority field";
        let n = fmt.parse(no_pri).unwrap();
        assert!(n.data.get("priority").is_none());
        assert!(n.data.get("level").is_none());
    }
}
