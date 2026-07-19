//! Bro / Zeek TSV (header-driven) detector.

use super::{LogFormat, NormalizedLog};
use serde_json::{json, Map, Value};

pub struct BroFormat;

/// True when a line looks like a Zeek/Bro separator or fields header.
pub fn is_bro_header_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("#separator")
        || t.starts_with("#fields")
        || t.starts_with("#types")
        || t.starts_with("#path")
}

fn parse_fields_header(line: &str) -> Option<Vec<String>> {
    let t = line.trim();
    let rest = t.strip_prefix("#fields")?;
    let fields: Vec<String> = rest.split_whitespace().map(|s| s.to_string()).collect();
    if fields.is_empty() {
        None
    } else {
        Some(fields)
    }
}

/// Parse a data row given column names (from a prior `#fields` line).
#[cfg(test)]
fn parse_bro_row(fields: &[String], line: &str) -> Option<Map<String, Value>> {
    if line.trim_start().starts_with('#') {
        return None;
    }
    let cols: Vec<&str> = line.split('\t').collect();
    if cols.is_empty() || fields.is_empty() {
        return None;
    }
    let mut map = Map::new();
    for (i, name) in fields.iter().enumerate() {
        let val = cols.get(i).copied().unwrap_or("-");
        map.insert(name.clone(), json!(val));
    }
    map.insert("_raw".into(), json!(line));
    if let Some(msg) = map
        .get("msg")
        .cloned()
        .or_else(|| map.get("message").cloned())
    {
        map.insert("msg".into(), msg);
    } else if let Some(uid) = map.get("uid").cloned() {
        map.insert("msg".into(), uid);
    }
    Some(map)
}

impl LogFormat for BroFormat {
    fn name(&self) -> &'static str {
        "bro_log"
    }

    fn detect(&self, line: &str) -> f32 {
        if is_bro_header_line(line) {
            return 0.85;
        }
        // Tab-separated with common Zeek columns without header context — weak.
        if line.contains('\t')
            && (line.contains("TCP") || line.contains("UDP") || line.contains("icmp"))
        {
            let tabs = line.matches('\t').count();
            if tabs >= 4 {
                return 0.55;
            }
        }
        0.0
    }

    fn parse(&self, line: &str) -> Option<NormalizedLog> {
        if is_bro_header_line(line) {
            let mut map = Map::new();
            map.insert("_raw".into(), json!(line.trim()));
            map.insert("msg".into(), json!(line.trim()));
            if let Some(fields) = parse_fields_header(line) {
                map.insert("_bro_fields".into(), json!(fields.join(",")));
            }
            map.insert("_format".into(), json!("bro_log"));
            return Some(NormalizedLog {
                data: Value::Object(map),
                format_id: "bro_log".into(),
            });
        }
        // Without session headers, accept generic TSV with many columns.
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 5 {
            return None;
        }
        let mut map = Map::new();
        for (i, c) in cols.iter().enumerate() {
            map.insert(format!("c{i}"), json!(c));
        }
        map.insert("_raw".into(), json!(line));
        map.insert("msg".into(), json!(cols.last().copied().unwrap_or("")));
        map.insert("_format".into(), json!("bro_log"));
        Some(NormalizedLog {
            data: Value::Object(map),
            format_id: "bro_log".into(),
        })
    }
}

/// Stateful helper for file ingest: remembers `#fields`.
#[cfg(test)]
#[derive(Debug, Default, Clone)]
struct BroSession {
    fields: Option<Vec<String>>,
}

#[cfg(test)]
impl BroSession {
    fn ingest_line(&mut self, line: &str) -> Option<NormalizedLog> {
        if let Some(fields) = parse_fields_header(line) {
            self.fields = Some(fields);
            return BroFormat.parse(line);
        }
        if is_bro_header_line(line) {
            return BroFormat.parse(line);
        }
        if let Some(fields) = &self.fields {
            let mut map = parse_bro_row(fields, line)?;
            map.insert("_format".into(), json!("bro_log"));
            return Some(NormalizedLog {
                data: Value::Object(map),
                format_id: "bro_log".into(),
            });
        }
        BroFormat.parse(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_fields_header() {
        let line = "#fields\tts\tuid\tid.orig_h\tid.orig_p\tproto";
        assert!(BroFormat.detect(line) >= 0.5);
        let n = BroFormat.parse(line).unwrap();
        assert_eq!(n.format_id, "bro_log");
        assert!(n.data["_bro_fields"].as_str().unwrap().contains("uid"));
    }

    #[test]
    fn session_parses_data_row() {
        let mut s = BroSession::default();
        s.ingest_line("#fields\tts\tuid\tproto\tmsg").unwrap();
        let n = s.ingest_line("1.0\tCabc123\tTCP\thello").unwrap();
        assert_eq!(n.format_id, "bro_log");
        assert_eq!(n.data["uid"], "Cabc123");
        assert_eq!(n.data["proto"], "TCP");
    }

    #[test]
    fn detect_and_parse_variants() {
        assert!(BroFormat.detect("#separator\t\\x09") >= 0.5);
        assert!(BroFormat.detect("#types\tstring\tcount") >= 0.5);
        assert!(BroFormat.detect("a\tb\tc\tTCP\tx\textra") >= 0.5);
        assert_eq!(BroFormat.detect("no tabs here"), 0.0);

        let header = BroFormat.parse("#path\tconn").unwrap();
        assert_eq!(header.data["_format"], "bro_log");
        assert!(BroFormat.parse("a\tb\tc").is_none());

        let tsv = BroFormat.parse("c0\tc1\tc2\tc3\tc4\tlast").unwrap();
        assert_eq!(tsv.data["c0"], "c0");
        assert_eq!(tsv.data["msg"], "last");

        let fields = vec!["message".into()];
        let row = parse_bro_row(&fields, "hello").unwrap();
        assert_eq!(row["msg"], "hello");

        let uid_fields = vec!["uid".into()];
        let uid_row = parse_bro_row(&uid_fields, "Cxyz").unwrap();
        assert_eq!(uid_row["msg"], "Cxyz");
        assert!(parse_bro_row(&uid_fields, "#skip").is_none());
    }
}
