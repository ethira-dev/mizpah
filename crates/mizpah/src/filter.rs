use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FilterOp {
    Eq,
    Neq,
    Contains,
    Gt,
    Lt,
    Exists,
    In,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterChip {
    pub path: String,
    pub op: FilterOp,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub values: Option<Vec<String>>,
}

/// Match filters against a log entry. Paths `service` and `level` are special:
/// - `service` compares against the entry's service tag
/// - `level` resolves `level` / `severity` / `lvl` in data (first present)
pub fn matches_all(service: &str, data: &Value, filters: &[FilterChip]) -> bool {
    filters.iter().all(|f| matches_one(service, data, f))
}

fn matches_one(service: &str, data: &Value, filter: &FilterChip) -> bool {
    let resolved = resolve_filter_value(service, data, &filter.path);
    match filter.op {
        FilterOp::Exists => resolved.is_some() && !resolved.as_ref().unwrap().is_null(),
        FilterOp::Eq => {
            let Some(v) = resolved else {
                return false;
            };
            let Some(expected) = filter.value.as_deref() else {
                return false;
            };
            value_as_string(&v) == expected
        }
        FilterOp::In => {
            let Some(v) = resolved else {
                return false;
            };
            let Some(values) = filter.values.as_ref() else {
                return false;
            };
            if values.is_empty() {
                return false;
            }
            let actual = value_as_string(&v);
            values.iter().any(|expected| actual == *expected)
        }
        FilterOp::Neq => {
            let Some(expected) = filter.value.as_deref() else {
                return true;
            };
            match resolved {
                None => true,
                Some(v) => value_as_string(&v) != expected,
            }
        }
        FilterOp::Contains => {
            let Some(v) = resolved else {
                return false;
            };
            let Some(needle) = filter.value.as_deref() else {
                return false;
            };
            value_as_string(&v)
                .to_lowercase()
                .contains(&needle.to_lowercase())
        }
        FilterOp::Gt => compare_num(resolved.as_ref(), filter.value.as_deref(), std::cmp::Ordering::Greater),
        FilterOp::Lt => compare_num(resolved.as_ref(), filter.value.as_deref(), std::cmp::Ordering::Less),
    }
}

/// Resolved value owned or borrowed from data. Service/level may synthesize a string Value.
fn resolve_filter_value(service: &str, data: &Value, path: &str) -> Option<Value> {
    match path {
        "service" => Some(Value::String(service.to_string())),
        "level" => level_of(data).map(Value::String),
        _ => get_path(data, path).cloned(),
    }
}

/// Mirror of UI `levelOf`: first of level / severity / lvl.
fn level_of(data: &Value) -> Option<String> {
    let Value::Object(map) = data else {
        return None;
    };
    for key in ["level", "severity", "lvl"] {
        if let Some(v) = map.get(key) {
            match v {
                Value::String(s) if !s.is_empty() => return Some(s.clone()),
                Value::Number(n) => return Some(n.to_string()),
                _ => {}
            }
        }
    }
    None
}

fn compare_num(
    resolved: Option<&Value>,
    expected: Option<&str>,
    want: std::cmp::Ordering,
) -> bool {
    let Some(v) = resolved else {
        return false;
    };
    let Some(exp) = expected else {
        return false;
    };
    let Ok(rhs) = exp.parse::<f64>() else {
        return false;
    };
    let Some(lhs) = value_as_f64(v) else {
        return false;
    };
    lhs.partial_cmp(&rhs) == Some(want)
}

fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn value_as_string(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Resolve a dotted path like `user.id` against a JSON value.
/// Array indices like `items[0].name` are also supported.
pub fn get_path<'a>(data: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(data);
    }
    let mut current = data;
    for segment in split_path(path) {
        match current {
            Value::Object(map) => {
                current = map.get(segment.as_str())?;
            }
            Value::Array(arr) => {
                let idx: usize = segment.parse().ok()?;
                current = arr.get(idx)?;
            }
            _ => return None,
        }
    }
    Some(current)
}

fn split_path(path: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut buf = String::new();
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '.' => {
                if !buf.is_empty() {
                    parts.push(std::mem::take(&mut buf));
                }
            }
            '[' => {
                if !buf.is_empty() {
                    parts.push(std::mem::take(&mut buf));
                }
                let mut idx = String::new();
                for c2 in chars.by_ref() {
                    if c2 == ']' {
                        break;
                    }
                    idx.push(c2);
                }
                if !idx.is_empty() {
                    parts.push(idx);
                }
            }
            _ => buf.push(c),
        }
    }
    if !buf.is_empty() {
        parts.push(buf);
    }
    parts
}

/// Parse filters from a JSON query string (array of chips).
pub fn parse_filters_param(raw: Option<&str>) -> Result<Vec<FilterChip>, String> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(raw).map_err(|e| format!("invalid filters: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dotted_path() {
        let data = json!({"user": {"id": 42}, "level": "error"});
        assert_eq!(get_path(&data, "user.id"), Some(&json!(42)));
        assert_eq!(get_path(&data, "level"), Some(&json!("error")));
    }

    #[test]
    fn filter_eq_and_contains() {
        let data = json!({"msg": "hello world", "level": "info"});
        assert!(matches_all(
            "api",
            &data,
            &[FilterChip {
                path: "level".into(),
                op: FilterOp::Eq,
                value: Some("info".into()),
                values: None,
            }]
        ));
        assert!(matches_all(
            "api",
            &data,
            &[FilterChip {
                path: "msg".into(),
                op: FilterOp::Contains,
                value: Some("WORLD".into()),
                values: None,
            }]
        ));
    }

    #[test]
    fn filter_in() {
        let data = json!({"level": "error"});
        assert!(matches_all(
            "api",
            &data,
            &[FilterChip {
                path: "level".into(),
                op: FilterOp::In,
                value: None,
                values: Some(vec!["warn".into(), "error".into()]),
            }]
        ));
        assert!(!matches_all(
            "api",
            &data,
            &[FilterChip {
                path: "level".into(),
                op: FilterOp::In,
                value: None,
                values: Some(vec!["info".into(), "debug".into()]),
            }]
        ));
        assert!(!matches_all(
            "api",
            &data,
            &[FilterChip {
                path: "level".into(),
                op: FilterOp::In,
                value: None,
                values: Some(vec![]),
            }]
        ));
    }

    #[test]
    fn filter_service_special_path() {
        let data = json!({"msg": "hi"});
        assert!(matches_all(
            "billing",
            &data,
            &[FilterChip {
                path: "service".into(),
                op: FilterOp::Eq,
                value: Some("billing".into()),
                values: None,
            }]
        ));
        assert!(!matches_all(
            "billing",
            &data,
            &[FilterChip {
                path: "service".into(),
                op: FilterOp::Eq,
                value: Some("api".into()),
                values: None,
            }]
        ));
        assert!(matches_all(
            "api",
            &data,
            &[FilterChip {
                path: "service".into(),
                op: FilterOp::In,
                value: None,
                values: Some(vec!["api".into(), "web".into()]),
            }]
        ));
    }

    #[test]
    fn filter_level_aliases() {
        let severity = json!({"severity": "warn"});
        assert!(matches_all(
            "api",
            &severity,
            &[FilterChip {
                path: "level".into(),
                op: FilterOp::Eq,
                value: Some("warn".into()),
                values: None,
            }]
        ));
        let lvl = json!({"lvl": 50});
        assert!(matches_all(
            "api",
            &lvl,
            &[FilterChip {
                path: "level".into(),
                op: FilterOp::Eq,
                value: Some("50".into()),
                values: None,
            }]
        ));
        // Prefer `level` over severity when both present
        let both = json!({"level": "error", "severity": "info"});
        assert!(matches_all(
            "api",
            &both,
            &[FilterChip {
                path: "level".into(),
                op: FilterOp::Eq,
                value: Some("error".into()),
                values: None,
            }]
        ));
    }
}
