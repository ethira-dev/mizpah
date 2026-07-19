//! Generic key:value / "message" fallback detector (low confidence).

use super::{LogFormat, NormalizedLog};
use serde_json::{json, Map, Value};

pub struct GenericFormat;

impl LogFormat for GenericFormat {
    fn name(&self) -> &'static str {
        "generic"
    }

    fn detect(&self, line: &str) -> f32 {
        let t = line.trim();
        if t.is_empty() || t.starts_with('{') {
            return 0.0;
        }
        // "LEVEL message" or bracketed timestamps
        let first = t.split_whitespace().next().unwrap_or("");
        let upper = first.to_ascii_uppercase();
        if matches!(
            upper.as_str(),
            "ERROR" | "WARN" | "WARNING" | "INFO" | "DEBUG" | "TRACE" | "FATAL"
        ) {
            // Above the 0.5 selection threshold so generic can win.
            return 0.55;
        }
        if t.starts_with('[') && t.contains(']') {
            return 0.35;
        }
        0.0
    }

    fn parse(&self, line: &str) -> Option<NormalizedLog> {
        let t = line.trim();
        let mut map = Map::new();
        map.insert("_raw".into(), Value::String(t.to_string()));
        let mut parts = t.splitn(2, char::is_whitespace);
        if let Some(first) = parts.next() {
            let upper = first.to_ascii_uppercase();
            if matches!(
                upper.as_str(),
                "ERROR" | "WARN" | "WARNING" | "INFO" | "DEBUG" | "TRACE" | "FATAL"
            ) {
                let level = if upper == "WARNING" {
                    "warn".to_string()
                } else {
                    upper.to_ascii_lowercase()
                };
                map.insert("level".into(), json!(level));
                if let Some(rest) = parts.next() {
                    map.insert("msg".into(), json!(rest));
                }
            } else {
                map.insert("msg".into(), json!(t));
            }
        }
        Some(NormalizedLog {
            data: Value::Object(map),
            format_id: "generic".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_branches() {
        let fmt = GenericFormat;
        assert_eq!(fmt.name(), "generic");
        assert_eq!(fmt.detect(""), 0.0);
        assert_eq!(fmt.detect(r#"{"a":1}"#), 0.0);
        assert_eq!(fmt.detect("[2024-01-01] something"), 0.35);
        assert_eq!(fmt.detect("ERROR boom"), 0.55);
        assert_eq!(fmt.detect("TRACE detail"), 0.55);
        assert_eq!(fmt.detect("plain message"), 0.0);
    }

    #[test]
    fn parse_level_and_fallback() {
        let fmt = GenericFormat;
        let n = fmt.parse("WARNING disk full").unwrap();
        assert_eq!(n.data["level"], "warn");
        assert_eq!(n.data["msg"], "disk full");

        let n = fmt.parse("INFO only").unwrap();
        assert_eq!(n.data["level"], "info");
        assert_eq!(n.data["msg"], "only");

        let n = fmt.parse("FATAL crash").unwrap();
        assert_eq!(n.data["level"], "fatal");

        let n = fmt.parse("no level prefix").unwrap();
        assert_eq!(n.data["msg"], "no level prefix");
        assert!(n.data.get("level").is_none());
    }
}
