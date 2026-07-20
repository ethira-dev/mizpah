//! Specialized detectors for syslog/logfmt-wrapped vendor lines that packs cannot win.

use super::{LogFormat, NormalizedLog};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

/// Heroku HTTP router (`heroku[router]: at=… method=…`).
pub struct HerokuRouterFormat;

fn heroku_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?x)
            ^
            (?:<(?P<pri>\d+)>)?
            (?:
                (?P<ts>[A-Z][a-z]{2}\s+\d{1,2}\s+\d{2}:\d{2}:\d{2})\s+
                (?P<host>\S+)\s+
            )?
            heroku\[router\]:\s+
            (?P<body>.*)
            $",
        )
        .expect("heroku router regex")
    })
}

fn parse_kv_body(body: &str) -> Map<String, Value> {
    let mut map = Map::new();
    for tok in body.split_whitespace() {
        if let Some((k, v)) = tok.split_once('=') {
            if k.is_empty() {
                continue;
            }
            let v = v.trim_matches('"');
            if let Ok(n) = v.parse::<i64>() {
                map.insert(k.to_string(), json!(n));
            } else {
                map.insert(k.to_string(), json!(v));
            }
        }
    }
    map
}

impl LogFormat for HerokuRouterFormat {
    fn name(&self) -> &'static str {
        "heroku_router_log"
    }

    fn detect(&self, line: &str) -> f32 {
        let t = line.trim();
        if t.contains("heroku[router]:") {
            0.95
        } else {
            0.0
        }
    }

    fn parse(&self, line: &str) -> Option<NormalizedLog> {
        let t = line.trim();
        let caps = heroku_re().captures(t)?;
        let body = caps.name("body").map(|m| m.as_str()).unwrap_or("");
        let mut map = parse_kv_body(body);
        map.insert("_raw".into(), json!(t));
        map.insert("msg".into(), json!(body));
        if let Some(ts) = caps.name("ts") {
            map.insert("timestamp".into(), json!(ts.as_str()));
        }
        if let Some(host) = caps.name("host") {
            map.insert("host".into(), json!(host.as_str()));
        }
        if let Some(at) = map.get("at").and_then(|v| v.as_str()) {
            let level = match at {
                "error" => "error",
                "warning" | "warn" => "warn",
                _ => "info",
            };
            map.insert("level".into(), json!(level));
        }
        Some(NormalizedLog {
            data: Value::Object(map),
            format_id: "heroku_router_log".into(),
        })
    }
}

/// F5 BIG-IP / ASM / tmm syslog-wrapped lines.
pub struct F5Format;

impl LogFormat for F5Format {
    fn name(&self) -> &'static str {
        "f5_log"
    }

    fn detect(&self, line: &str) -> f32 {
        let t = line.trim();
        let lower = t.to_ascii_lowercase();
        if lower.contains("asm:")
            || t.contains("tmm[")
            || lower.contains("f5-")
            || t.contains("Rule /Common/")
            || t.contains("ltm ")
        {
            0.92
        } else {
            0.0
        }
    }

    fn parse(&self, line: &str) -> Option<NormalizedLog> {
        if self.detect(line) < 0.5 {
            return None;
        }
        let t = line.trim();
        Some(NormalizedLog {
            data: json!({
                "msg": t,
                "_raw": t,
            }),
            format_id: "f5_log".into(),
        })
    }
}

/// HashiCorp Consul agent lines (`consul[…]:`).
pub struct ConsulFormat;

impl LogFormat for ConsulFormat {
    fn name(&self) -> &'static str {
        "consul_log"
    }

    fn detect(&self, line: &str) -> f32 {
        let t = line.trim();
        if t.contains("consul[") || t.contains("consul.http") {
            return 0.92;
        }
        // Consul agent leveled lines: "[INFO]  agent: …"
        if (t.contains("[INFO]  agent:")
            || t.contains("[WARN]  agent:")
            || t.contains("[ERROR] agent:")
            || t.contains("[DEBUG] agent:"))
            && !t.contains("nomad")
        {
            return 0.92;
        }
        0.0
    }

    fn parse(&self, line: &str) -> Option<NormalizedLog> {
        if self.detect(line) < 0.5 {
            return None;
        }
        let t = line.trim();
        let level = if t.contains("[ERROR]") {
            "error"
        } else if t.contains("[WARN]") {
            "warn"
        } else if t.contains("[DEBUG]") {
            "debug"
        } else {
            "info"
        };
        Some(NormalizedLog {
            data: json!({
                "level": level,
                "msg": t,
                "_raw": t,
            }),
            format_id: "consul_log".into(),
        })
    }
}

/// HashiCorp Nomad agent lines.
pub struct NomadFormat;

impl LogFormat for NomadFormat {
    fn name(&self) -> &'static str {
        "nomad_log"
    }

    fn detect(&self, line: &str) -> f32 {
        let t = line.trim();
        if t.contains("nomad[")
            || t.contains("nomad.http")
            || ((t.contains("[INFO] ") || t.contains("[WARN] ") || t.contains("[ERROR] "))
                && (t.contains(" client:")
                    || t.contains(" server:")
                    || t.contains(" worker:")
                    || t.contains(" alloc:")))
        {
            0.92
        } else {
            0.0
        }
    }

    fn parse(&self, line: &str) -> Option<NormalizedLog> {
        if self.detect(line) < 0.5 {
            return None;
        }
        let t = line.trim();
        let level = if t.contains("[ERROR]") {
            "error"
        } else if t.contains("[WARN]") {
            "warn"
        } else if t.contains("[DEBUG]") {
            "debug"
        } else {
            "info"
        };
        Some(NormalizedLog {
            data: json!({
                "level": level,
                "msg": t,
                "_raw": t,
            }),
            format_id: "nomad_log".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heroku_beats_logfmt_shape() {
        let line = r#"Jan  1 00:00:00 host heroku[router]: at=info method=GET path="/" host=example.com request_id=abc fwd="1.2.3.4" dyno=web.1 connect=1ms service=2ms status=200 bytes=123"#;
        let fmt = HerokuRouterFormat;
        assert!(fmt.detect(line) >= 0.95);
        let n = fmt.parse(line).unwrap();
        assert_eq!(n.format_id, "heroku_router_log");
        assert_eq!(n.data["method"], "GET");
        assert_eq!(n.data["status"], 200);
        assert_eq!(n.data["level"], "info");
    }

    #[test]
    fn f5_detects_asm() {
        let line = r#"Jan  1 00:00:00 bigip1 ASM:dvc_time=2020-01-01 attack_type=SQL"#;
        assert!(F5Format.detect(line) >= 0.92);
        assert_eq!(F5Format.parse(line).unwrap().format_id, "f5_log");
    }

    #[test]
    fn consul_detects_agent() {
        let line = r#"2020-01-01T00:00:00.000Z [INFO]  agent: Synced service: service=web"#;
        assert!(ConsulFormat.detect(line) >= 0.92);
    }

    #[test]
    fn nomad_detects_client() {
        let line = r#"2020-01-01T00:00:00.000Z [INFO]  client: node registration complete"#;
        assert!(NomadFormat.detect(line) >= 0.92);
    }
}
