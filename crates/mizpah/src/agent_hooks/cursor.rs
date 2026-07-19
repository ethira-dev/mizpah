//! Cursor hooks.json merge / remove / attach / detach.

use super::shared::is_managed_command;
use super::state::HookSource;
use serde_json::{json, Value};

pub(crate) const CURSOR_EVENTS: &[&str] = &[
    "sessionStart",
    "sessionEnd",
    "preToolUse",
    "postToolUse",
    "postToolUseFailure",
    "subagentStart",
    "subagentStop",
    "beforeShellExecution",
    "afterShellExecution",
    "beforeMCPExecution",
    "afterMCPExecution",
    "beforeReadFile",
    "afterFileEdit",
    "beforeSubmitPrompt",
    "preCompact",
    "stop",
    "afterAgentResponse",
    "afterAgentThought",
    "workspaceOpen",
];

/// Merge Mizpah handlers into a Cursor `hooks.json` document.
pub(crate) fn merge_cursor_hooks(existing: &str, command: &str) -> Result<(String, bool), String> {
    let mut root: Value = if existing.trim().is_empty() {
        json!({ "version": 1, "hooks": {} })
    } else {
        serde_json::from_str(existing).map_err(|e| format!("invalid Cursor hooks.json: {e}"))?
    };

    let obj = root
        .as_object_mut()
        .ok_or_else(|| "Cursor hooks.json root must be an object".to_string())?;
    if !obj.contains_key("version") {
        obj.insert("version".into(), json!(1));
    }
    let hooks = obj.entry("hooks".to_string()).or_insert_with(|| json!({}));
    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| "Cursor hooks.json `hooks` must be an object".to_string())?;

    let mut changed = false;
    let handler = json!({ "command": command });
    for event in CURSOR_EVENTS {
        let arr = hooks_obj
            .entry((*event).to_string())
            .or_insert_with(|| json!([]));
        let list = arr
            .as_array_mut()
            .ok_or_else(|| format!("Cursor hooks.{event} must be an array"))?;
        let already = list.iter().any(|h| {
            h.get("command")
                .and_then(|c| c.as_str())
                .is_some_and(|c| c == command || is_managed_command(c, HookSource::Cursor))
        });
        if already {
            // Refresh command path if an older managed entry exists with a different binary path.
            for h in list.iter_mut() {
                if let Some(c) = h.get("command").and_then(|c| c.as_str()) {
                    if is_managed_command(c, HookSource::Cursor) && c != command {
                        h.as_object_mut()
                            .map(|o| o.insert("command".into(), json!(command)));
                        changed = true;
                    }
                }
            }
        } else {
            list.push(handler.clone());
            changed = true;
        }
    }

    let out = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    Ok((format!("{out}\n"), changed))
}

/// Remove Mizpah handlers from a Cursor `hooks.json` document.
pub(crate) fn remove_cursor_hooks(existing: &str) -> Result<(String, bool), String> {
    if existing.trim().is_empty() {
        return Ok((String::new(), false));
    }
    let mut root: Value =
        serde_json::from_str(existing).map_err(|e| format!("invalid Cursor hooks.json: {e}"))?;
    let Some(hooks_obj) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return Ok((existing.to_string(), false));
    };

    let mut changed = false;
    let keys: Vec<String> = hooks_obj.keys().cloned().collect();
    for key in keys {
        let Some(arr) = hooks_obj.get_mut(&key).and_then(|a| a.as_array_mut()) else {
            continue;
        };
        let before = arr.len();
        arr.retain(|h| {
            !h.get("command")
                .and_then(|c| c.as_str())
                .is_some_and(|c| is_managed_command(c, HookSource::Cursor))
        });
        if arr.len() != before {
            changed = true;
        }
        if arr.is_empty() {
            hooks_obj.remove(&key);
            changed = true;
        }
    }

    let out = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    Ok((format!("{out}\n"), changed))
}

pub async fn run_attach_cursor(
    service: Option<String>,
    host: String,
    port: u16,
) -> Result<(), String> {
    super::run_attach_source(HookSource::Cursor, service, host, port).await
}

pub fn run_detach_cursor() -> Result<(), String> {
    super::run_detach_source(HookSource::Cursor)
}

#[cfg(test)]
mod tests {
    use super::super::shared::{is_managed_command, managed_command, HOOK_MARKER};
    use super::*;
    use std::path::Path;

    #[test]
    fn merge_cursor_hooks_idempotent() {
        let cmd = managed_command(Path::new("/bin/mzp"), HookSource::Cursor);
        let (once, changed) = merge_cursor_hooks("", &cmd).unwrap();
        assert!(changed);
        let (twice, changed2) = merge_cursor_hooks(&once, &cmd).unwrap();
        assert!(!changed2);
        assert_eq!(once, twice);
        assert!(once.contains("preToolUse"));
        assert!(!once.contains("beforeTabFileRead"));
        let v: Value = serde_json::from_str(&once).unwrap();
        let hooks = v["hooks"].as_object().unwrap();
        for event in CURSOR_EVENTS {
            let arr = hooks[*event].as_array().unwrap();
            let count = arr
                .iter()
                .filter(|h| {
                    h["command"]
                        .as_str()
                        .is_some_and(|c| is_managed_command(c, HookSource::Cursor))
                })
                .count();
            assert_eq!(count, 1, "event {event}");
        }
    }

    #[test]
    fn remove_cursor_preserves_user_hooks() {
        let cmd = managed_command(Path::new("/bin/mzp"), HookSource::Cursor);
        let existing = r#"{
          "version": 1,
          "hooks": {
            "afterFileEdit": [
              { "command": "./format.sh" },
              { "command": "/bin/mzp __hook-forward --source cursor" }
            ]
          }
        }"#;
        let (out, changed) = remove_cursor_hooks(existing).unwrap();
        assert!(changed);
        assert!(out.contains("format.sh"));
        assert!(!is_managed_command(&out, HookSource::Cursor) || !out.contains(HOOK_MARKER));
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v["hooks"]["afterFileEdit"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["command"], "./format.sh");
        let _ = cmd;
    }

    #[test]
    fn cursor_events_exclude_tab() {
        assert!(!CURSOR_EVENTS.contains(&"beforeTabFileRead"));
        assert!(!CURSOR_EVENTS.contains(&"afterTabFileEdit"));
    }

    #[test]
    fn merge_cursor_invalid_json_errors() {
        let err =
            merge_cursor_hooks("{bad", "/bin/mzp __hook-forward --source cursor").unwrap_err();
        assert!(err.contains("invalid Cursor hooks.json"));
    }

    #[test]
    fn merge_cursor_refreshes_stale_command_path() {
        let cmd = managed_command(Path::new("/new/mzp"), HookSource::Cursor);
        let existing = r#"{
          "version": 1,
          "hooks": {
            "stop": [{ "command": "/old/mzp __hook-forward --source cursor" }]
          }
        }"#;
        let (out, changed) = merge_cursor_hooks(existing, &cmd).unwrap();
        assert!(changed);
        assert!(out.contains("/new/mzp"));
    }

    #[test]
    fn remove_cursor_empty_input() {
        let (out, changed) = remove_cursor_hooks("").unwrap();
        assert!(!changed);
        assert!(out.is_empty());
    }

    #[test]
    fn merge_cursor_root_not_object_errors() {
        let err = merge_cursor_hooks("[]", "cmd").unwrap_err();
        assert!(err.contains("root must be an object"));
    }

    #[test]
    fn merge_cursor_event_not_array_errors() {
        let err = merge_cursor_hooks(r#"{"hooks":{"stop":"x"}}"#, "cmd").unwrap_err();
        assert!(err.contains("must be an array"));
    }

    #[test]
    fn merge_cursor_no_change_when_command_current() {
        let cmd = managed_command(Path::new("/bin/mzp"), HookSource::Cursor);
        let (once, _) = merge_cursor_hooks("", &cmd).unwrap();
        let (_, changed) = merge_cursor_hooks(&once, &cmd).unwrap();
        assert!(!changed);
    }

    #[test]
    fn merge_cursor_hooks_field_not_object_errors() {
        let err = merge_cursor_hooks(r#"{"hooks":"nope"}"#, "cmd").unwrap_err();
        assert!(err.contains("`hooks` must be an object"));
    }

    #[test]
    fn remove_cursor_invalid_json_errors() {
        assert!(remove_cursor_hooks("{").is_err());
    }
}
