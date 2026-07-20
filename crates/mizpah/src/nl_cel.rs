//! Deterministic natural-language → CEL compiler (no LLM).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NlCelResult {
    pub cel: String,
    pub confidence: f32,
    pub warnings: Vec<String>,
}

/// Compile a natural-language log query into CEL.
pub fn compile_nl(text: &str) -> NlCelResult {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return NlCelResult {
            cel: String::new(),
            confidence: 1.0,
            warnings: Vec::new(),
        };
    }

    let lower = trimmed.to_ascii_lowercase();
    let mut parts: Vec<String> = Vec::new();
    let mut confidence: f32 = 0.9;
    let mut warnings = Vec::new();

    if lower.contains("error") {
        parts.push(r#"level == "error""#.into());
    }
    if lower.contains("warn") {
        parts.push(r#"(level == "warn" || level == "warning")"#.into());
    }

    if let Some(svc) = extract_after(&lower, "from ") {
        let svc = svc.split_whitespace().next().unwrap_or("").trim();
        if !svc.is_empty() {
            parts.push(format!(r#"service == "{}""#, escape_cel_string(svc)));
        }
    }

    if let Some(word) = extract_contains(trimmed) {
        parts.push(format!(
            r#"(msg.contains("{0}") || _raw.contains("{0}"))"#,
            escape_cel_string(&word)
        ));
    }

    if parts.is_empty() {
        confidence = 0.4;
        warnings.push("fell back to substring match on msg/_raw".into());
        let escaped = escape_cel_string(trimmed);
        return NlCelResult {
            cel: format!(r#"msg.contains("{escaped}") || _raw.contains("{escaped}")"#),
            confidence,
            warnings,
        };
    }

    NlCelResult {
        cel: parts.join(" && "),
        confidence,
        warnings,
    }
}

fn escape_cel_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn extract_after<'a>(lower: &'a str, marker: &str) -> Option<&'a str> {
    lower.find(marker).map(|i| &lower[i + marker.len()..])
}

fn extract_contains(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let Some(rest) = extract_after(&lower, "contains ") else {
        return None;
    };
    let rest_orig = &text[text.len() - rest.len()..];
    let rest_trim = rest_orig.trim();
    if let Some(inner) = rest_trim.strip_prefix('"').and_then(|s| {
        let end = s.find('"')?;
        Some(s[..end].to_string())
    }) {
        return Some(inner);
    }
    if let Some(inner) = rest_trim.strip_prefix('\'').and_then(|s| {
        let end = s.find('\'')?;
        Some(s[..end].to_string())
    }) {
        return Some(inner);
    }
    Some(
        rest_trim
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string(),
    )
    .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_errors() {
        let r = compile_nl("errors");
        assert!(r.cel.contains("error"));
        assert!(r.confidence > 0.5);
    }

    #[test]
    fn compile_fallback() {
        let r = compile_nl("weird unique phrase xyz");
        assert!(r.cel.contains("msg.contains"));
        assert!(r.cel.contains("_raw.contains"));
    }
}
