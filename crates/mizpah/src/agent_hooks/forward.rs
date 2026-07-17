//! Hidden `__hook-forward`: stdin hook JSON → hub ingest.

use super::state::{load_state, HookSource};
use crate::hub;
use crate::mzp_meta::MzpMeta;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::io::{self, Read};
use std::time::Duration;
use tracing::debug;

pub(crate) const MAX_STRING_BYTES: usize = 64 * 1024;
const FORWARD_HTTP_TIMEOUT: Duration = Duration::from_secs(2);

/// Hidden `__hook-forward`: read hook JSON from stdin, POST to hub, exit 0, no stdout.
pub async fn run_hook_forward(source: HookSource) {
    // Never write to stdout — Claude SessionStart / UserPromptSubmit inject stdout as context.
    let result = forward_once(source).await;
    if let Err(err) = result {
        debug!(error = %err, source = source.as_str(), "hook forward failed (fail-open)");
    }
}

async fn forward_once(source: HookSource) -> Result<(), String> {
    let state = load_state().map_err(|e| e.to_string())?;
    let src = match source {
        HookSource::Cursor => state.cursor,
        HookSource::Claude => state.claude,
    };
    let Some(src) = src.filter(|s| s.enabled) else {
        return Ok(());
    };

    let mut raw = String::new();
    io::stdin()
        .read_to_string(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.trim().is_empty() {
        return Ok(());
    }

    let event: Value =
        serde_json::from_str(raw.trim()).unwrap_or_else(|_| json!({ "_raw": raw.trim() }));
    let cwd_hint = extract_cwd(&event);
    let envelope = build_envelope(source, event);
    let line = serde_json::to_string(&envelope).map_err(|e| e.to_string())?;

    let mut mzp = MzpMeta::capture();
    if let Some(cwd) = cwd_hint {
        mzp = mzp.with_cwd(cwd);
    }

    let url = format!("{}/api/ingest", hub::hub_url(&src.host, src.port));
    let client = reqwest::Client::builder()
        .timeout(FORWARD_HTTP_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;

    #[derive(Serialize)]
    struct Body<'a> {
        service: &'a str,
        line: &'a str,
        mzp: &'a MzpMeta,
    }

    let _ = client
        .post(&url)
        .json(&Body {
            service: &src.service,
            line: &line,
            mzp: &mzp,
        })
        .send()
        .await;
    Ok(())
}

fn extract_cwd(event: &Value) -> Option<String> {
    if let Some(cwd) = event.get("cwd").and_then(|v| v.as_str()) {
        if !cwd.is_empty() {
            return Some(cwd.to_string());
        }
    }
    if let Some(roots) = event.get("workspace_roots").and_then(|v| v.as_array()) {
        if let Some(r) = roots.first().and_then(|v| v.as_str()) {
            if !r.is_empty() {
                return Some(r.to_string());
            }
        }
    }
    None
}

/// Build the hub log envelope from a hook event payload.
pub(crate) fn build_envelope(source: HookSource, mut event: Value) -> Value {
    let kind = event
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let truncated_fields = truncate_strings(&mut event, MAX_STRING_BYTES);
    let level = derive_level(&kind, &event);
    let msg = derive_msg(source.as_str(), &kind, &event);

    let mut out = Map::new();
    if let Some(obj) = event.as_object() {
        for (k, v) in obj {
            let key = match k.as_str() {
                "source" => "hookSource",
                "kind" => "hookKind",
                "level" => "hookLevel",
                "msg" => "hookMsg",
                other => other,
            };
            out.insert(key.to_string(), v.clone());
        }
    } else {
        out.insert("_value".into(), event);
    }

    out.insert("source".into(), json!(source.as_str()));
    out.insert("kind".into(), json!(kind));
    out.insert("level".into(), json!(level));
    out.insert("msg".into(), json!(msg));
    if !truncated_fields.is_empty() {
        out.insert("truncated".into(), json!(true));
        out.insert("truncatedFields".into(), json!(truncated_fields));
    }

    Value::Object(out)
}

fn derive_level(kind: &str, event: &Value) -> &'static str {
    let kind_l = kind.to_ascii_lowercase();
    if kind_l.contains("failure") || kind_l == "stopfailure" || kind_l == "permissiondenied" {
        return "error";
    }
    if let Some(ft) = event.get("failure_type").and_then(|v| v.as_str()) {
        if !ft.is_empty() {
            return "error";
        }
    }
    if let Some(status) = event.get("status").and_then(|v| v.as_str()) {
        let s = status.to_ascii_lowercase();
        if s == "error" {
            return "error";
        }
        if s == "aborted" || s == "denied" {
            return "warn";
        }
    }
    if let Some(reason) = event.get("reason").and_then(|v| v.as_str()) {
        let r = reason.to_ascii_lowercase();
        if r == "error" {
            return "error";
        }
        if r.contains("abort") || r.contains("close") {
            return "warn";
        }
    }
    "info"
}

fn derive_msg(source: &str, kind: &str, event: &Value) -> String {
    let detail = event
        .get("tool_name")
        .and_then(|v| v.as_str())
        .or_else(|| event.get("command").and_then(|v| v.as_str()))
        .or_else(|| event.get("file_path").and_then(|v| v.as_str()))
        .or_else(|| {
            event
                .get("tool_input")
                .and_then(|t| t.get("command"))
                .and_then(|v| v.as_str())
        })
        .or_else(|| event.get("prompt").and_then(|v| v.as_str()))
        .or_else(|| event.get("task").and_then(|v| v.as_str()))
        .or_else(|| event.get("hookSource").and_then(|v| v.as_str()))
        .or_else(|| {
            // Claude SessionStart `source` before rename — still on event here.
            event.get("source").and_then(|v| v.as_str())
        })
        .unwrap_or("");

    let detail = truncate_str(detail, 120);
    if detail.is_empty() {
        format!("{source} {kind}")
    } else {
        format!("{source} {kind}: {detail}")
    }
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn truncate_strings(value: &mut Value, max_bytes: usize) -> Vec<String> {
    let mut fields = Vec::new();
    truncate_strings_at(value, max_bytes, "", &mut fields);
    fields
}

fn truncate_strings_at(value: &mut Value, max_bytes: usize, path: &str, fields: &mut Vec<String>) {
    match value {
        Value::String(s) => {
            if s.len() > max_bytes {
                const ELLIPSIS: &str = "…";
                let mut cut = max_bytes.saturating_sub(ELLIPSIS.len());
                while cut > 0 && !s.is_char_boundary(cut) {
                    cut -= 1;
                }
                s.truncate(cut);
                s.push_str(ELLIPSIS);
                debug_assert!(s.len() <= max_bytes);
                if !path.is_empty() {
                    fields.push(path.to_string());
                }
            }
        }
        Value::Array(arr) => {
            for (i, item) in arr.iter_mut().enumerate() {
                let child = if path.is_empty() {
                    format!("[{i}]")
                } else {
                    format!("{path}[{i}]")
                };
                truncate_strings_at(item, max_bytes, &child, fields);
            }
        }
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                let child = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                truncate_strings_at(v, max_bytes, &child, fields);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_renames_source_conflict_and_sets_level() {
        let event = json!({
            "hook_event_name": "SessionStart",
            "source": "startup",
            "session_id": "abc"
        });
        let env = build_envelope(HookSource::Claude, event);
        assert_eq!(env["source"], "claude");
        assert_eq!(env["hookSource"], "startup");
        assert_eq!(env["kind"], "SessionStart");
        assert_eq!(env["level"], "info");
        assert!(env["msg"].as_str().unwrap().contains("SessionStart"));
    }

    #[test]
    fn envelope_failure_is_error_and_truncates() {
        let big = "x".repeat(MAX_STRING_BYTES + 100);
        let event = json!({
            "hook_event_name": "postToolUseFailure",
            "failure_type": "timeout",
            "content": big
        });
        let env = build_envelope(HookSource::Cursor, event);
        assert_eq!(env["level"], "error");
        assert_eq!(env["truncated"], true);
        assert!(env["truncatedFields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f.as_str() == Some("content")));
        assert!(env["content"].as_str().unwrap().len() <= MAX_STRING_BYTES);
    }
}
