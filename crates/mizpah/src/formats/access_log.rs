//! Common / combined Apache-style access log parser.

use super::{LogFormat, NormalizedLog};
use regex::Regex;
use serde_json::json;
use std::sync::OnceLock;

pub struct AccessLogFormat;

fn combined_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?x)
            ^
            (?P<ip>\S+)\s+
            \S+\s+
            \S+\s+
            \[(?P<time>[^\]]+)\]\s+
            "(?P<request>[^"]*)"\s+
            (?P<status>\d{3})\s+
            (?P<size>\S+)
            (?:\s+"(?P<referer>[^"]*)"\s+"(?P<agent>[^"]*)")?
            "#,
        )
        .expect("access log regex")
    })
}

impl LogFormat for AccessLogFormat {
    fn name(&self) -> &'static str {
        "access_log"
    }

    fn detect(&self, line: &str) -> f32 {
        if combined_re().is_match(line.trim()) {
            0.8
        } else {
            0.0
        }
    }

    fn parse(&self, line: &str) -> Option<NormalizedLog> {
        let caps = combined_re().captures(line.trim())?;
        let status: u16 = caps.name("status")?.as_str().parse().ok()?;
        let level = if status >= 500 {
            "error"
        } else if status >= 400 {
            "warn"
        } else {
            "info"
        };
        let request = caps.name("request").map(|m| m.as_str()).unwrap_or("");
        let mut method = "";
        let mut path = request;
        let mut parts = request.split_whitespace();
        if let Some(m) = parts.next() {
            method = m;
            if let Some(p) = parts.next() {
                path = p;
            }
        }
        let size_raw = caps.name("size").map(|m| m.as_str()).unwrap_or("-");
        let size = size_raw.parse::<u64>().ok();
        let mut obj = json!({
            "ip": caps.name("ip").map(|m| m.as_str()).unwrap_or(""),
            "timestamp": caps.name("time").map(|m| m.as_str()).unwrap_or(""),
            "request": request,
            "method": method,
            "path": path,
            "status": status,
            "level": level,
            "_raw": line.trim(),
        });
        if let Some(n) = size {
            obj["size"] = json!(n);
        }
        if let Some(r) = caps.name("referer") {
            obj["referer"] = json!(r.as_str());
        }
        if let Some(a) = caps.name("agent") {
            obj["user_agent"] = json!(a.as_str());
        }
        Some(NormalizedLog {
            data: obj,
            format_id: "access_log".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_combined() {
        let line = r#"127.0.0.1 - - [10/Oct/2000:13:55:36 -0700] "GET /index.html HTTP/1.0" 200 2326 "http://x" "Mozilla""#;
        let n = AccessLogFormat.parse(line).unwrap();
        assert_eq!(n.data["status"], 200);
        assert_eq!(n.data["method"], "GET");
        assert_eq!(n.data["path"], "/index.html");
        assert_eq!(n.data["level"], "info");
        assert_eq!(n.data["size"], 2326);
        assert_eq!(n.data["referer"], "http://x");
        assert_eq!(n.data["user_agent"], "Mozilla");
    }

    #[test]
    fn detect_and_name() {
        let fmt = AccessLogFormat;
        assert_eq!(fmt.name(), "access_log");
        let line = r#"127.0.0.1 - - [10/Oct/2000:13:55:36 -0700] "GET / HTTP/1.0" 200 0"#;
        assert!(fmt.detect(line) >= 0.5);
        assert_eq!(fmt.detect("not an access log"), 0.0);
    }

    #[test]
    fn status_levels_and_minimal_line() {
        let fmt = AccessLogFormat;
        let warn = r#"1.2.3.4 - - [10/Oct/2000:13:55:36 -0700] "GET /missing HTTP/1.0" 404 -"#;
        let err = r#"1.2.3.4 - - [10/Oct/2000:13:55:36 -0700] "GET /fail HTTP/1.0" 503 -"#;
        let method_only = r#"1.2.3.4 - - [10/Oct/2000:13:55:36 -0700] "GET HTTP/1.0" 200 0"#;
        let no_path = r#"1.2.3.4 - - [10/Oct/2000:13:55:36 -0700] "GET" 200 -"#;
        assert_eq!(fmt.parse(warn).unwrap().data["level"], "warn");
        assert_eq!(fmt.parse(err).unwrap().data["level"], "error");
        assert!(fmt.parse(warn).unwrap().data.get("size").is_none());
        assert_eq!(fmt.parse(method_only).unwrap().data["path"], "HTTP/1.0");
        assert_eq!(fmt.parse(no_path).unwrap().data["method"], "GET");
        assert_eq!(fmt.parse(no_path).unwrap().data["path"], "GET");
        assert!(fmt.parse("garbage").is_none());
    }
}
