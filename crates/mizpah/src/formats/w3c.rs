//! W3C Extended Log File Format (header-driven).

use super::{LogFormat, NormalizedLog};
use serde_json::{json, Map, Value};

pub struct W3cFormat;

pub fn is_w3c_directive(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("#Version:")
        || t.starts_with("#Fields:")
        || t.starts_with("#Date:")
        || t.starts_with("#Start-Date:")
        || t.starts_with("#Software:")
        || t.starts_with("#Remark:")
}

fn parse_fields_directive(line: &str) -> Option<Vec<String>> {
    let t = line.trim();
    let rest = t.strip_prefix("#Fields:")?;
    let fields: Vec<String> = rest.split_whitespace().map(|s| s.to_string()).collect();
    if fields.is_empty() {
        None
    } else {
        Some(fields)
    }
}

#[cfg(test)]
fn parse_w3c_row(fields: &[String], line: &str) -> Option<Map<String, Value>> {
    if line.trim_start().starts_with('#') {
        return None;
    }
    let cols: Vec<&str> = line.split_whitespace().collect();
    if cols.len() < fields.len().saturating_sub(2) || fields.is_empty() {
        // Allow minor column drift but require a reasonable match.
        if cols.len() < 3 {
            return None;
        }
    }
    let mut map = Map::new();
    for (i, name) in fields.iter().enumerate() {
        let val = cols.get(i).copied().unwrap_or("-");
        // W3C field names may contain '-', '(', ')' — keep as-is.
        map.insert(name.clone(), json!(val));
    }
    map.insert("_raw".into(), json!(line));
    // Common status / cs-method helpers
    if let Some(status) = map
        .get("sc-status")
        .or_else(|| map.get("status"))
        .cloned()
    {
        if let Ok(code) = status
            .as_str()
            .unwrap_or("")
            .parse::<u64>()
        {
            map.insert("status".into(), json!(code));
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
    if let Some(m) = map.get("cs-method").cloned() {
        map.insert("method".into(), m);
    }
    if let Some(u) = map.get("cs-uri-stem").cloned() {
        map.insert("path".into(), u.clone());
        map.insert("msg".into(), u);
    } else if let Some(u) = map.get("cs-uri").cloned() {
        map.insert("msg".into(), u);
    }
    Some(map)
}

impl LogFormat for W3cFormat {
    fn name(&self) -> &'static str {
        "w3c_log"
    }

    fn detect(&self, line: &str) -> f32 {
        if is_w3c_directive(line) {
            return 0.9;
        }
        0.0
    }

    fn parse(&self, line: &str) -> Option<NormalizedLog> {
        if !is_w3c_directive(line) {
            return None;
        }
        let mut map = Map::new();
        map.insert("_raw".into(), json!(line.trim()));
        map.insert("msg".into(), json!(line.trim()));
        if let Some(fields) = parse_fields_directive(line) {
            map.insert("_w3c_fields".into(), json!(fields.join(" ")));
        }
        map.insert("_format".into(), json!("w3c_log"));
        Some(NormalizedLog {
            data: Value::Object(map),
            format_id: "w3c_log".into(),
        })
    }
}

#[cfg(test)]
#[derive(Debug, Default, Clone)]
struct W3cSession {
    fields: Option<Vec<String>>,
}

#[cfg(test)]
impl W3cSession {
    fn ingest_line(&mut self, line: &str) -> Option<NormalizedLog> {
        if let Some(fields) = parse_fields_directive(line) {
            self.fields = Some(fields);
            return W3cFormat.parse(line);
        }
        if is_w3c_directive(line) {
            return W3cFormat.parse(line);
        }
        if let Some(fields) = &self.fields {
            let mut map = parse_w3c_row(fields, line)?;
            map.insert("_format".into(), json!("w3c_log"));
            return Some(NormalizedLog {
                data: Value::Object(map),
                format_id: "w3c_log".into(),
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_fields_directive() {
        let line = "#Fields: date time s-ip cs-method cs-uri-stem sc-status";
        assert!(W3cFormat.detect(line) >= 0.5);
        let n = W3cFormat.parse(line).unwrap();
        assert_eq!(n.format_id, "w3c_log");
    }

    #[test]
    fn session_parses_row() {
        let mut s = W3cSession::default();
        s.ingest_line("#Fields: date time cs-method cs-uri-stem sc-status")
            .unwrap();
        let n = s
            .ingest_line("2020-01-01 00:00:00 GET /index.html 200")
            .unwrap();
        assert_eq!(n.format_id, "w3c_log");
        assert_eq!(n.data["status"], 200);
        assert_eq!(n.data["method"], "GET");
        assert_eq!(n.data["path"], "/index.html");
        assert_eq!(n.data["level"], "info");
    }

    #[test]
    fn directives_and_row_variants() {
        assert!(is_w3c_directive("#Version: 1.0"));
        assert!(is_w3c_directive("#Date: 2020-01-01"));
        assert!(is_w3c_directive("#Start-Date: x"));
        assert!(is_w3c_directive("#Software: IIS"));
        assert!(is_w3c_directive("#Remark: test"));
        assert_eq!(W3cFormat.detect("plain row"), 0.0);
        assert!(parse_fields_directive("#Fields:").is_none());

        let fields = vec!["status".into(), "cs-uri".into()];
        let row = parse_w3c_row(&fields, "404 /missing").unwrap();
        assert_eq!(row["status"], 404);
        assert_eq!(row["level"], "warn");
        assert_eq!(row["msg"], "/missing");

        let err_fields = vec!["sc-status".into()];
        let err_row = parse_w3c_row(&err_fields, "500").unwrap();
        assert_eq!(err_row["level"], "error");
        assert!(parse_w3c_row(&fields, "# comment").is_none());
        let short_fields = vec!["a".into(), "b".into(), "c".into(), "d".into(), "e".into()];
        assert!(parse_w3c_row(&short_fields, "x y").is_none());

        let mut s = W3cSession::default();
        s.ingest_line("#Software: test").unwrap();
        assert!(s.ingest_line("2020-01-01 GET / 200").is_none());
    }
}
