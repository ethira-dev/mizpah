//! logfmt key=value parser.

use super::{LogFormat, NormalizedLog};
use serde_json::{Map, Number, Value};

pub struct LogfmtFormat;

/// Parse a logfmt line into a JSON object. Returns `None` if no key=value pairs found.
pub fn parse_logfmt(line: &str) -> Option<Map<String, Value>> {
    let mut map = Map::new();
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            // Not key=value — abort if we have nothing yet
            if map.is_empty() {
                return None;
            }
            // Skip token
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            continue;
        }
        let key = std::str::from_utf8(&bytes[key_start..i]).ok()?.to_string();
        i += 1; // skip '='
        if i >= bytes.len() {
            map.insert(key, Value::String(String::new()));
            break;
        }
        let value = if bytes[i] == b'"' {
            i += 1;
            let mut out = String::new();
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' if i + 1 < bytes.len() => {
                        out.push(bytes[i + 1] as char);
                        i += 2;
                    }
                    b'"' => {
                        i += 1;
                        break;
                    }
                    b => {
                        out.push(b as char);
                        i += 1;
                    }
                }
            }
            Value::String(out)
        } else {
            let val_start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let raw = std::str::from_utf8(&bytes[val_start..i]).ok()?;
            coerce_value(raw)
        };
        map.insert(key, value);
    }
    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

fn coerce_value(raw: &str) -> Value {
    if raw == "true" {
        return Value::Bool(true);
    }
    if raw == "false" {
        return Value::Bool(false);
    }
    if let Ok(i) = raw.parse::<i64>() {
        return Value::Number(i.into());
    }
    if let Ok(f) = raw.parse::<f64>() {
        if let Some(n) = Number::from_f64(f) {
            return Value::Number(n);
        }
    }
    Value::String(raw.to_string())
}

impl LogFormat for LogfmtFormat {
    fn name(&self) -> &'static str {
        "logfmt"
    }

    fn detect(&self, line: &str) -> f32 {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('{') {
            return 0.0;
        }
        let mut pairs = 0u32;
        let mut tokens = 0u32;
        for tok in trimmed.split_whitespace() {
            tokens += 1;
            if let Some((k, _)) = tok.split_once('=') {
                if !k.is_empty()
                    && k.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
                {
                    pairs += 1;
                }
            } else if tok.contains('=') {
                // quoted value case counted loosely
                pairs += 1;
            }
        }
        if pairs == 0 {
            return 0.0;
        }
        let ratio = pairs as f32 / tokens.max(1) as f32;
        if pairs >= 2 && ratio >= 0.5 {
            0.85
        } else if pairs >= 1 && ratio >= 0.4 {
            0.55
        } else {
            0.25
        }
    }

    fn parse(&self, line: &str) -> Option<NormalizedLog> {
        let map = parse_logfmt(line)?;
        Some(NormalizedLog {
            data: Value::Object(map),
            format_id: "logfmt".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_and_numbers() {
        let m = parse_logfmt(r#"level=warn msg="a b" n=3"#).unwrap();
        assert_eq!(m["level"], "warn");
        assert_eq!(m["msg"], "a b");
        assert_eq!(m["n"], 3);
    }

    #[test]
    fn coerce_bools_float_and_empty_value() {
        let m = parse_logfmt(r#"ok=true off=false ratio=1.5 tail="#).unwrap();
        assert_eq!(m["ok"], true);
        assert_eq!(m["off"], false);
        assert_eq!(m["ratio"].as_f64().unwrap(), 1.5);
        assert_eq!(m["tail"], "");
        assert!(parse_logfmt("not key value").is_none());
    }

    #[test]
    fn quoted_escape_and_skip_tokens() {
        let m = parse_logfmt(r#"x=y level=info msg="a \"b\"""#).unwrap();
        assert_eq!(m["level"], "info");
        assert_eq!(m["msg"], r#"a "b""#);
    }

    #[test]
    fn detect_confidence_tiers() {
        let fmt = LogfmtFormat;
        assert_eq!(fmt.detect(""), 0.0);
        assert_eq!(fmt.detect(r#"{"x":1}"#), 0.0);
        assert!(fmt.detect("a=1 b=2 c=3") >= 0.8);
        assert!(fmt.detect("a=1 x") >= 0.5);
        assert!(fmt.detect("a=1 only") >= 0.25);
    }

    #[test]
    fn parse_skips_stray_token_after_pairs() {
        let m = parse_logfmt("a=1 stray b=2").unwrap();
        assert_eq!(m["a"], 1);
        assert_eq!(m["b"], 2);
    }
}
