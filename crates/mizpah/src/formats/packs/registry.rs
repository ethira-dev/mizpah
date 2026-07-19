//! Compile and query vendored format-v1 packs.

use super::normalize::{
    apply_text_aliases, is_mizpah_primary_pack, json_path, map_level, mizpah_format_id,
    normalize_json_object,
};
use crate::formats::NormalizedLog;
use include_dir::{include_dir, Dir};
use pcre2::bytes::{Regex, RegexBuilder};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::OnceLock;

static PACKS_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/formats/packs");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatKind {
    Text,
    Json,
}

#[derive(Debug, Deserialize)]
struct RawPackFile {
    #[serde(rename = "$schema", default)]
    _schema: Option<String>,
    #[serde(flatten)]
    packs: HashMap<String, RawPack>,
}

#[derive(Debug, Deserialize)]
struct RawPack {
    #[serde(default, rename = "file-type")]
    file_type: Option<String>,
    #[serde(default)]
    json: Option<bool>,
    #[serde(default)]
    converter: Option<Value>,
    #[serde(default)]
    regex: HashMap<String, RawRegex>,
    #[cfg(test)]
    #[serde(default)]
    sample: Vec<RawSample>,
    #[serde(default, rename = "timestamp-field")]
    timestamp_field: Option<String>,
    #[serde(default, rename = "level-field")]
    level_field: Option<String>,
    #[serde(default, rename = "body-field")]
    body_field: Option<String>,
    #[serde(default)]
    level: Map<String, Value>,
    #[serde(default, rename = "file-pattern")]
    file_pattern: Option<String>,
    #[serde(default, rename = "line-format")]
    line_format: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct RawRegex {
    pattern: String,
}

#[cfg(test)]
#[derive(Debug, Deserialize)]
struct RawSample {
    line: String,
    #[serde(default)]
    level: Option<Value>,
}

pub struct CompiledRegex {
    pub re: Regex,
    pub pattern_len: usize,
}

pub struct CompiledPack {
    pub pack_id: String,
    pub kind: FormatKind,
    pub regexes: Vec<CompiledRegex>,
    #[cfg(test)]
    pub samples: Vec<(String, Option<Value>)>,
    pub timestamp_field: Option<String>,
    pub level_field: Option<String>,
    pub body_field: Option<String>,
    pub level_map: Map<String, Value>,
    /// Optional path regex (reserved for file-pattern filtering).
    _file_pattern: Option<Regex>,
    /// Higher = more specific (more capture groups / longer patterns).
    pub specificity: f32,
    pub line_format_fields: Vec<String>,
}

pub struct PackRegistry {
    pub packs: Vec<CompiledPack>,
    /// pack_id → index
    by_id: HashMap<String, usize>,
}

impl PackRegistry {
    pub fn get(&self, pack_id: &str) -> Option<&CompiledPack> {
        self.by_id.get(pack_id).map(|&i| &self.packs[i])
    }

    pub fn text_packs(&self) -> impl Iterator<Item = &CompiledPack> {
        self.packs.iter().filter(|p| p.kind == FormatKind::Text)
    }

    pub fn json_packs(&self) -> impl Iterator<Item = &CompiledPack> {
        self.packs.iter().filter(|p| p.kind == FormatKind::Json)
    }
}

fn compile_regex(pattern: &str) -> Result<Regex, String> {
    RegexBuilder::new()
        .utf(true)
        .ucp(true)
        .build(pattern)
        .map_err(|e| format!("{e}"))
}

fn load_registry() -> PackRegistry {
    let mut packs = Vec::new();
    let mut errors = Vec::new();

    for file in PACKS_DIR.files() {
        let path = file.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".json") {
            continue;
        }
        let Ok(text) = std::str::from_utf8(file.contents()) else {
            errors.push(format!("{name}: invalid utf-8"));
            continue;
        };
        let parsed: RawPackFile = match serde_json::from_str(text) {
            Ok(p) => p,
            Err(e) => {
                errors.push(format!("{name}: parse {e}"));
                continue;
            }
        };
        for (pack_id, raw) in parsed.packs {
            if raw.converter.is_some() {
                // Converter packs must not be registered.
                continue;
            }
            let is_json = raw.file_type.as_deref() == Some("json") || raw.json == Some(true);
            let kind = if is_json {
                FormatKind::Json
            } else {
                FormatKind::Text
            };

            let mut regexes = Vec::new();
            for (rname, rdef) in &raw.regex {
                match compile_regex(&rdef.pattern) {
                    Ok(re) => regexes.push(CompiledRegex {
                        pattern_len: rdef.pattern.len(),
                        re,
                    }),
                    Err(e) => errors.push(format!("{pack_id}/{rname}: {e}")),
                }
            }
            if kind == FormatKind::Text && regexes.is_empty() {
                errors.push(format!("{pack_id}: text pack has no compiled regexes"));
                continue;
            }

            let file_pattern = match &raw.file_pattern {
                Some(p) => match compile_regex(p) {
                    Ok(re) => Some(re),
                    Err(e) => {
                        errors.push(format!("{pack_id} file-pattern: {e}"));
                        None
                    }
                },
                None => None,
            };

            #[cfg(test)]
            let samples: Vec<(String, Option<Value>)> =
                raw.sample.into_iter().map(|s| (s.line, s.level)).collect();

            let line_format_fields: Vec<String> = raw
                .line_format
                .iter()
                .filter_map(|v| {
                    v.as_object()
                        .and_then(|o| o.get("field"))
                        .and_then(|f| f.as_str())
                        .map(|s| s.to_string())
                })
                .collect();

            let mut specificity = 0.5_f32;
            for cr in &regexes {
                let named = cr.re.capture_names().iter().filter(|n| n.is_some()).count();
                specificity += 0.01 * named as f32;
                specificity += 0.0001 * cr.pattern_len as f32;
            }
            specificity += 0.02 * line_format_fields.len() as f32;
            if file_pattern.is_some() {
                specificity += 0.05;
            }

            packs.push(CompiledPack {
                pack_id,
                kind,
                regexes,
                #[cfg(test)]
                samples,
                timestamp_field: raw.timestamp_field,
                level_field: raw.level_field,
                body_field: raw.body_field,
                level_map: raw.level,
                _file_pattern: file_pattern,
                specificity,
                line_format_fields,
            });
        }
    }

    if !errors.is_empty() {
        // Fail loud in tests; in runtime log and continue with what compiled.
        for e in &errors {
            tracing::error!(error = %e, "format pack compile error");
        }
        #[cfg(test)]
        panic!("format pack compile errors:\n{}", errors.join("\n"));
    }

    packs.sort_by(|a, b| {
        b.specificity
            .partial_cmp(&a.specificity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let by_id = packs
        .iter()
        .enumerate()
        .map(|(i, p)| (p.pack_id.clone(), i))
        .collect();

    PackRegistry { packs, by_id }
}

pub fn registry() -> &'static PackRegistry {
    static REG: OnceLock<PackRegistry> = OnceLock::new();
    REG.get_or_init(load_registry)
}

#[cfg(test)]
pub fn loaded_pack_ids() -> Vec<String> {
    registry().packs.iter().map(|p| p.pack_id.clone()).collect()
}

fn capture_to_map(re: &Regex, line: &str) -> Option<Map<String, Value>> {
    let caps = re.captures(line.as_bytes()).ok()??;
    let mut map = Map::new();
    for (i, name) in re.capture_names().iter().enumerate() {
        let Some(name) = name else { continue };
        let Some(m) = caps.get(i) else { continue };
        let Ok(s) = std::str::from_utf8(m.as_bytes()) else {
            continue;
        };
        map.insert(name.clone(), json!(s));
    }
    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

impl CompiledPack {
    pub fn match_text(&self, line: &str) -> Option<Map<String, Value>> {
        for cr in &self.regexes {
            if let Some(map) = capture_to_map(&cr.re, line) {
                return Some(map);
            }
        }
        None
    }

    pub fn detect_text(&self, line: &str) -> f32 {
        if self.match_text(line).is_some() {
            (0.55 + self.specificity * 0.01).clamp(0.55, 0.95)
        } else {
            0.0
        }
    }

    pub fn parse_text(&self, line: &str) -> Option<NormalizedLog> {
        let mut map = self.match_text(line)?;
        map.insert("_raw".into(), json!(line));

        if let Some(lf) = &self.level_field {
            if let Some(raw) = map.get(lf).cloned() {
                if let Some(level) = map_level(&raw, &self.level_map) {
                    map.insert("level".into(), json!(level));
                }
            }
        } else if !self.level_map.is_empty() {
            // Often levels off `body`
            let probe = map
                .get("body")
                .or_else(|| map.get("msg"))
                .cloned()
                .unwrap_or(Value::String(line.to_string()));
            if let Some(level) = map_level(&probe, &self.level_map) {
                // Only set if pattern-based map found a named level (heuristic: not the whole body)
                if level.len() < 24 {
                    map.insert("level".into(), json!(level));
                }
            }
        }

        apply_text_aliases(&mut map);
        let stable = mizpah_format_id(&self.pack_id);
        map.insert("_format".into(), json!(stable));
        map.insert("_pack_format".into(), json!(&self.pack_id));

        Some(NormalizedLog {
            data: Value::Object(map),
            format_id: stable.to_string(),
        })
    }

    pub fn json_confidence(&self, obj: &Map<String, Value>) -> f32 {
        let root = Value::Object(obj.clone());
        if let Some(tf) = &self.timestamp_field {
            if json_path(&root, tf).is_none() {
                return 0.0;
            }
        } else {
            // No timestamp-field: require at least two line-format fields present.
            let hits = self
                .line_format_fields
                .iter()
                .filter(|f| !f.starts_with("__") && json_path(&root, f).is_some())
                .count();
            if hits < 2 {
                return 0.0;
            }
            return (0.55 + 0.05 * hits as f32).clamp(0.55, 0.9);
        }

        let mut score = 0.6_f32;
        let field_hits = self
            .line_format_fields
            .iter()
            .filter(|f| !f.starts_with("__") && json_path(&root, f).is_some())
            .count();
        score += 0.03 * field_hits as f32;
        if let Some(lf) = &self.level_field {
            if json_path(&root, lf).is_some() {
                score += 0.05;
            }
        }
        score.clamp(0.55, 0.95)
    }

    pub fn parse_json(&self, obj: &Map<String, Value>) -> Option<NormalizedLog> {
        if self.json_confidence(obj) < 0.5 {
            return None;
        }
        let map = normalize_json_object(
            obj,
            &self.pack_id,
            self.timestamp_field.as_deref(),
            self.level_field.as_deref(),
            self.body_field.as_deref(),
            &self.level_map,
        );
        let stable = mizpah_format_id(&self.pack_id);
        Some(NormalizedLog {
            data: Value::Object(map),
            format_id: stable.to_string(),
        })
    }
}

/// Best-matching JSON pack (excludes Mizpah-primary packs so bunyan/pino/journald stay local).
pub fn classify_pack_json(obj: &Map<String, Value>) -> Option<NormalizedLog> {
    let reg = registry();
    let mut best: Option<(f32, &CompiledPack)> = None;
    for pack in reg.json_packs() {
        if is_mizpah_primary_pack(&pack.pack_id) {
            continue;
        }
        let c = pack.json_confidence(obj);
        if c >= 0.5 {
            match best {
                None => best = Some((c, pack)),
                Some((bc, _)) if c > bc => best = Some((c, pack)),
                _ => {}
            }
        }
    }
    best.and_then(|(_, p)| p.parse_json(obj))
}

/// Detect among non-primary text packs. Returns (confidence, pack_id).
pub fn detect_pack_text(line: &str) -> Option<(f32, &'static str)> {
    let reg = registry();
    let mut best: Option<(f32, &CompiledPack)> = None;
    for pack in reg.text_packs() {
        if is_mizpah_primary_pack(&pack.pack_id) {
            continue;
        }
        let c = pack.detect_text(line);
        if c >= 0.5 {
            match best {
                None => best = Some((c, pack)),
                Some((bc, _)) if c > bc => best = Some((c, pack)),
                _ => {}
            }
        }
    }
    best.map(|(c, p)| (c, p.pack_id.as_str()))
}

pub fn parse_pack_text(line: &str) -> Option<NormalizedLog> {
    let (c, id) = detect_pack_text(line)?;
    if c < 0.5 {
        return None;
    }
    registry().get(id)?.parse_text(line)
}

/// Parse with a locked format hint (pack id or stable Mizpah id).
pub fn parse_with_format_hint(line: &str, hint: &str) -> Option<NormalizedLog> {
    let reg = registry();
    let pack = reg.get(hint).or_else(|| {
        // Resolve stable → pack id
        let mapped = match hint {
            "syslog" => "syslog_log",
            "bunyan" => "bunyan_log",
            "pino" => "pino_log",
            "journald" => "journald_json_log",
            "slog" => "slog_json_log",
            "zerolog" => "zerolog_json_log",
            "logrus" => "logrus_json_log",
            "structlog" => "structlog_json_log",
            other => other,
        };
        reg.get(mapped)
    })?;

    match pack.kind {
        FormatKind::Text => pack.parse_text(line),
        FormatKind::Json => {
            let Value::Object(obj) = serde_json::from_str(line.trim()).ok()? else {
                return None;
            };
            pack.parse_json(&obj)
        }
    }
}
