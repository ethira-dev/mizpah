use crate::models::{LogEntry, PropertyInfo, PropertyValueInfo};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Default)]
pub(crate) struct PathMeta {
    types: HashSet<String>,
    sample_values: Vec<String>,
}

pub(crate) fn discover_paths_into(
    value: &Value,
    prefix: &str,
    map: &mut HashMap<String, PathMeta>,
) {
    match value {
        Value::Object(obj) => {
            for (key, child) in obj {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                record_path(&path, child, map);
                if child.is_object() {
                    discover_paths_into(child, &path, map);
                } else if let Value::Array(arr) = child {
                    for (i, item) in arr.iter().enumerate().take(5) {
                        if item.is_object() {
                            let item_path = format!("{path}[{i}]");
                            discover_paths_into(item, &item_path, map);
                        }
                    }
                }
            }
        }
        other => {
            if !prefix.is_empty() {
                record_path(prefix, other, map);
            }
        }
    }
}

fn record_path(path: &str, value: &Value, map: &mut HashMap<String, PathMeta>) {
    let meta = map.entry(path.to_string()).or_default();
    let type_name = match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    meta.types.insert(type_name.to_string());

    if meta.sample_values.len() < 20 {
        let sample = match value {
            Value::String(s) => {
                if s.len() > 80 {
                    format!("{}…", &s[..80])
                } else {
                    s.clone()
                }
            }
            Value::Null => "null".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            Value::Array(_) | Value::Object(_) => return,
        };
        if !meta.sample_values.contains(&sample) {
            meta.sample_values.push(sample);
        }
    }
}

pub(crate) fn paths_to_info(map: &HashMap<String, PathMeta>) -> Vec<PropertyInfo> {
    let mut infos: Vec<PropertyInfo> = map
        .iter()
        .map(|(path, meta)| {
            let mut types: Vec<String> = meta.types.iter().cloned().collect();
            types.sort();
            PropertyInfo {
                path: path.clone(),
                types,
                sample_values: meta.sample_values.clone(),
                count: 0,
                values: Vec::new(),
            }
        })
        .collect();
    infos.sort_by(|a, b| a.path.cmp(&b.path));
    infos
}

pub(crate) fn push_service_property(
    infos: &mut Vec<PropertyInfo>,
    services: &HashMap<String, u64>,
    service_filter: Option<&str>,
) {
    infos.retain(|p| p.path != "service");

    let mut names: Vec<String> = match service_filter {
        Some(svc) if !svc.is_empty() && svc != "*" => {
            if services.contains_key(svc) {
                vec![svc.to_string()]
            } else {
                Vec::new()
            }
        }
        _ => services.keys().cloned().collect(),
    };
    if names.is_empty() {
        return;
    }
    names.sort_by_key(|a| a.to_ascii_lowercase());

    infos.push(PropertyInfo {
        path: "service".into(),
        types: vec!["string".into()],
        sample_values: names,
        count: 0,
        values: Vec::new(),
    });
    infos.sort_by(|a, b| a.path.cmp(&b.path));
}

pub(crate) fn filter_properties_by_query(
    infos: Vec<PropertyInfo>,
    needle: &str,
) -> Vec<PropertyInfo> {
    let mut out = Vec::new();
    for mut info in infos {
        let path_match = info.path.to_ascii_lowercase().contains(needle);
        if path_match {
            out.push(info);
            continue;
        }
        info.sample_values
            .retain(|v| v.to_ascii_lowercase().contains(needle));
        if !info.sample_values.is_empty() {
            out.push(info);
        }
    }
    out
}

pub(crate) fn annotate_property_counts(
    entries: &VecDeque<LogEntry>,
    service_filter: Option<&str>,
    infos: &mut [PropertyInfo],
) {
    if infos.is_empty() {
        return;
    }

    let mut prop_counts = vec![0u64; infos.len()];
    let mut value_counts: Vec<Vec<u64>> = infos
        .iter()
        .map(|info| vec![0u64; info.sample_values.len()])
        .collect();

    for entry in entries {
        if let Some(svc) = service_filter {
            if !svc.is_empty() && svc != "*" && entry.service != svc {
                continue;
            }
        }
        for (i, info) in infos.iter().enumerate() {
            if !value_exists(entry, &info.path) {
                continue;
            }
            prop_counts[i] += 1;
            for (j, sample) in info.sample_values.iter().enumerate() {
                if value_matches(entry, &info.path, sample) {
                    value_counts[i][j] += 1;
                }
            }
        }
    }

    for (i, info) in infos.iter_mut().enumerate() {
        info.count = prop_counts[i];
        info.values = info
            .sample_values
            .iter()
            .zip(value_counts[i].iter())
            .map(|(value, &count)| PropertyValueInfo {
                value: value.clone(),
                count,
            })
            .collect();
    }
}

/// Resolve a dotted / bracket path in log JSON (`user.id`, `items[0].name`).
pub(crate) fn get_at_path<'a>(data: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = data;
    let mut rest = path;
    while !rest.is_empty() {
        if let Some(stripped) = rest.strip_prefix('[') {
            let end = stripped.find(']')?;
            let idx: usize = stripped[..end].parse().ok()?;
            cur = cur.as_array()?.get(idx)?;
            rest = &stripped[end + 1..];
            if let Some(r) = rest.strip_prefix('.') {
                rest = r;
            }
        } else {
            let end = rest.find(['.', '[']).unwrap_or(rest.len());
            let key = &rest[..end];
            cur = cur.as_object()?.get(key)?;
            rest = &rest[end..];
            if let Some(r) = rest.strip_prefix('.') {
                rest = r;
            }
        }
    }
    Some(cur)
}

fn value_exists(entry: &LogEntry, path: &str) -> bool {
    if path == "service" {
        return true;
    }
    get_at_path(&entry.data, path).is_some()
}

fn sample_matches(actual: &Value, sample: &str) -> bool {
    if let Some(prefix) = sample.strip_suffix('…') {
        return actual
            .as_str()
            .map(|s| s.starts_with(prefix))
            .unwrap_or(false);
    }
    match actual {
        Value::Null => sample == "null",
        Value::Bool(b) => sample == b.to_string(),
        Value::Number(n) => sample == n.to_string(),
        Value::String(s) => sample == s,
        _ => false,
    }
}

fn value_matches(entry: &LogEntry, path: &str, sample: &str) -> bool {
    if path == "service" {
        return entry.service == sample;
    }
    match get_at_path(&entry.data, path) {
        Some(actual) => sample_matches(actual, sample),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn get_at_path_nested_and_array() {
        let data = json!({
            "user": { "id": "42" },
            "items": [{ "name": "a" }, { "name": "b" }]
        });
        assert_eq!(get_at_path(&data, "user.id"), Some(&json!("42")));
        assert_eq!(get_at_path(&data, "items[1].name"), Some(&json!("b")));
        assert_eq!(get_at_path(&data, "missing"), None);
    }
}
