use crate::models::{LogEntry, PropertyInfo, PropertyValueInfo};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Default)]
pub(crate) struct PathMeta {
    types: HashSet<String>,
    sample_values: Vec<String>,
    /// Buffered entries that currently have this path.
    count: u64,
    /// Occurrence counts for each sample value in the current buffer.
    value_counts: HashMap<String, u64>,
}

/// Discover paths in `value`, optionally bumping occurrence counts.
/// Returns `true` when the schema changed (new path, type, or sample).
pub(crate) fn discover_paths_into(
    value: &Value,
    prefix: &str,
    map: &mut HashMap<String, PathMeta>,
    bump_counts: bool,
) -> bool {
    match value {
        Value::Object(obj) => {
            let mut changed = false;
            for (key, child) in obj {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                changed |= record_path(&path, child, map, bump_counts);
                if child.is_object() {
                    changed |= discover_paths_into(child, &path, map, bump_counts);
                } else if let Value::Array(arr) = child {
                    for (i, item) in arr.iter().enumerate().take(5) {
                        if item.is_object() {
                            let item_path = format!("{path}[{i}]");
                            changed |= discover_paths_into(item, &item_path, map, bump_counts);
                        }
                    }
                }
            }
            changed
        }
        other => {
            if prefix.is_empty() {
                false
            } else {
                record_path(prefix, other, map, bump_counts)
            }
        }
    }
}

/// Decrement occurrence counts for paths present in an evicted entry.
pub(crate) fn decrement_counts_for_entry(data: &Value, map: &mut HashMap<String, PathMeta>) {
    walk_leaf_paths(data, "", &mut |path, value| {
        let Some(meta) = map.get_mut(path) else {
            return;
        };
        meta.count = meta.count.saturating_sub(1);
        if let Some(sample) = sample_of(value) {
            if let Some(c) = meta.value_counts.get_mut(&sample) {
                *c = c.saturating_sub(1);
            }
        }
    });
}

fn walk_leaf_paths(value: &Value, prefix: &str, f: &mut dyn FnMut(&str, &Value)) {
    match value {
        Value::Object(obj) => {
            for (key, child) in obj {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                f(&path, child);
                if child.is_object() {
                    walk_leaf_paths(child, &path, f);
                } else if let Value::Array(arr) = child {
                    for (i, item) in arr.iter().enumerate().take(5) {
                        if item.is_object() {
                            let item_path = format!("{path}[{i}]");
                            walk_leaf_paths(item, &item_path, f);
                        }
                    }
                }
            }
        }
        other if !prefix.is_empty() => f(prefix, other),
        _ => {}
    }
}

fn record_path(
    path: &str,
    value: &Value,
    map: &mut HashMap<String, PathMeta>,
    bump_counts: bool,
) -> bool {
    let meta = map.entry(path.to_string()).or_default();
    let mut schema_changed = false;

    let type_name = match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    if meta.types.insert(type_name.to_string()) {
        schema_changed = true;
    }

    if let Some(sample) = sample_of(value) {
        if !meta.sample_values.contains(&sample) && meta.sample_values.len() < 20 {
            meta.sample_values.push(sample.clone());
            schema_changed = true;
        }
        if bump_counts {
            meta.count += 1;
            *meta.value_counts.entry(sample).or_insert(0) += 1;
        }
    } else if bump_counts {
        // Objects/arrays still contribute to path presence counts.
        meta.count += 1;
    }

    schema_changed
}

fn sample_of(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(if s.len() > 80 {
            format!("{}…", &s[..80])
        } else {
            s.clone()
        }),
        Value::Null => Some("null".to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Array(_) | Value::Object(_) => None,
    }
}

pub(crate) fn paths_to_info(map: &HashMap<String, PathMeta>) -> Vec<PropertyInfo> {
    let mut infos: Vec<PropertyInfo> = map
        .iter()
        .map(|(path, meta)| {
            let mut types: Vec<String> = meta.types.iter().cloned().collect();
            types.sort();
            let mut sample_values = meta.sample_values.clone();
            sample_values.sort_by_key(|a| a.to_ascii_lowercase());
            let values = sample_values
                .iter()
                .map(|value| PropertyValueInfo {
                    value: value.clone(),
                    count: meta.value_counts.get(value).copied().unwrap_or(0),
                })
                .collect();
            PropertyInfo {
                path: path.clone(),
                types,
                sample_values,
                count: meta.count,
                values,
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

    let total: u64 = match service_filter {
        Some(svc) if !svc.is_empty() && svc != "*" => services.get(svc).copied().unwrap_or(0),
        _ => services.values().sum(),
    };

    let values = names
        .iter()
        .map(|name| PropertyValueInfo {
            value: name.clone(),
            count: services.get(name).copied().unwrap_or(0),
        })
        .collect();

    infos.push(PropertyInfo {
        path: "service".into(),
        types: vec!["string".into()],
        sample_values: names,
        count: total,
        values,
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
        info.values
            .retain(|v| v.value.to_ascii_lowercase().contains(needle));
        if !info.sample_values.is_empty() {
            out.push(info);
        }
    }
    out
}

/// Resolve a dotted / bracket path in log JSON (`user.id`, `items[0].name`).
#[cfg_attr(not(test), allow(dead_code))]
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

/// Rebuild path metas (schema + counts) from the current buffer.
pub(crate) fn rebuild_properties_from_entries(
    entries: &VecDeque<LogEntry>,
) -> HashMap<String, PathMeta> {
    let mut properties = HashMap::new();
    for entry in entries {
        let _ = discover_paths_into(&entry.data, "", &mut properties, true);
    }
    properties
}

pub(crate) fn rebuild_properties_by_service(
    entries: &VecDeque<LogEntry>,
) -> HashMap<String, HashMap<String, PathMeta>> {
    let mut by_service: HashMap<String, HashMap<String, PathMeta>> = HashMap::new();
    for entry in entries {
        let map = by_service.entry(entry.service.clone()).or_default();
        let _ = discover_paths_into(&entry.data, "", map, true);
    }
    by_service
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

    #[test]
    fn discover_reports_schema_change_once() {
        let mut map = HashMap::new();
        let data = json!({"level": "info", "msg": "hi"});
        assert!(discover_paths_into(&data, "", &mut map, true));
        assert!(!discover_paths_into(&data, "", &mut map, true));
        assert_eq!(map["level"].count, 2);
    }
}
