//! Claude Code settings.json merge / remove / attach / detach.

use super::shared::is_managed_command;
use super::state::HookSource;
use serde_json::{json, Value};

const CLAUDE_HOOK_TIMEOUT_SECS: u64 = 5;

pub(crate) const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "Setup",
    "InstructionsLoaded",
    "UserPromptSubmit",
    "UserPromptExpansion",
    "PreToolUse",
    "PermissionRequest",
    "PermissionDenied",
    "PostToolUse",
    "PostToolUseFailure",
    "PostToolBatch",
    "Notification",
    "SubagentStart",
    "SubagentStop",
    "TaskCreated",
    "TaskCompleted",
    "Stop",
    "StopFailure",
    "TeammateIdle",
    "ConfigChange",
    "CwdChanged",
    "WorktreeRemove",
    "PreCompact",
    "PostCompact",
    "Elicitation",
    "ElicitationResult",
    "SessionEnd",
];

/// Merge Mizpah handlers into a Claude Code `settings.json` document.
pub(crate) fn merge_claude_hooks(existing: &str, command: &str) -> Result<(String, bool), String> {
    let mut root: Value = if existing.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(existing).map_err(|e| format!("invalid Claude settings.json: {e}"))?
    };

    let obj = root
        .as_object_mut()
        .ok_or_else(|| "Claude settings.json root must be an object".to_string())?;
    let hooks = obj.entry("hooks".to_string()).or_insert_with(|| json!({}));
    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| "Claude settings.json `hooks` must be an object".to_string())?;

    let mut changed = false;
    let handler = json!({
        "type": "command",
        "command": command,
        "timeout": CLAUDE_HOOK_TIMEOUT_SECS,
    });

    for event in CLAUDE_EVENTS {
        let groups = hooks_obj
            .entry((*event).to_string())
            .or_insert_with(|| json!([]));
        let groups_arr = groups
            .as_array_mut()
            .ok_or_else(|| format!("Claude hooks.{event} must be an array"))?;

        let mut found = false;
        for group in groups_arr.iter_mut() {
            let Some(inner) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) else {
                continue;
            };
            for h in inner.iter_mut() {
                if h.get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(|c| is_managed_command(c, HookSource::Claude))
                {
                    found = true;
                    if let Some(o) = h.as_object_mut() {
                        if o.get("command").and_then(|c| c.as_str()) != Some(command) {
                            o.insert("command".into(), json!(command));
                            changed = true;
                        }
                        o.insert("type".into(), json!("command"));
                        o.insert("timeout".into(), json!(CLAUDE_HOOK_TIMEOUT_SECS));
                        // Drop args if present — command is a full shell string.
                        o.remove("args");
                    }
                }
            }
        }

        if !found {
            groups_arr.push(json!({ "hooks": [handler.clone()] }));
            changed = true;
        }
    }

    let out = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    Ok((format!("{out}\n"), changed))
}

/// Remove Mizpah handlers from a Claude Code `settings.json` document.
pub(crate) fn remove_claude_hooks(existing: &str) -> Result<(String, bool), String> {
    if existing.trim().is_empty() {
        return Ok((String::new(), false));
    }
    let mut root: Value =
        serde_json::from_str(existing).map_err(|e| format!("invalid Claude settings.json: {e}"))?;
    let Some(hooks_obj) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return Ok((existing.to_string(), false));
    };

    let mut changed = false;
    let keys: Vec<String> = hooks_obj.keys().cloned().collect();
    for key in keys {
        let Some(groups) = hooks_obj.get_mut(&key).and_then(|a| a.as_array_mut()) else {
            continue;
        };
        for group in groups.iter_mut() {
            let Some(inner) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) else {
                continue;
            };
            let before = inner.len();
            inner.retain(|h| {
                !h.get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(|c| is_managed_command(c, HookSource::Claude))
            });
            if inner.len() != before {
                changed = true;
            }
        }
        groups.retain(|g| {
            g.get("hooks")
                .and_then(|h| h.as_array())
                .is_none_or(|a| !a.is_empty())
        });
        if groups.is_empty() {
            hooks_obj.remove(&key);
            changed = true;
        }
    }

    if hooks_obj.is_empty() {
        if let Some(root_obj) = root.as_object_mut() {
            root_obj.remove("hooks");
            changed = true;
        }
    }

    let out = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    Ok((format!("{out}\n"), changed))
}

pub async fn run_attach_claude(
    service: Option<String>,
    host: String,
    port: u16,
) -> Result<(), String> {
    super::run_attach_source(HookSource::Claude, service, host, port).await
}

pub fn run_detach_claude() -> Result<(), String> {
    super::run_detach_source(HookSource::Claude)
}

#[cfg(test)]
mod tests {
    use super::super::shared::managed_command;
    use super::*;
    use std::path::Path;

    #[test]
    fn merge_claude_skips_dangerous_events() {
        let cmd = managed_command(Path::new("/bin/mzp"), HookSource::Claude);
        let (out, _) = merge_claude_hooks("", &cmd).unwrap();
        assert!(!out.contains("WorktreeCreate"));
        assert!(!out.contains("FileChanged"));
        assert!(!out.contains("MessageDisplay"));
        assert!(out.contains("PreToolUse"));
        assert!(out.contains("SessionStart"));
        let v: Value = serde_json::from_str(&out).unwrap();
        let groups = v["hooks"]["PreToolUse"].as_array().unwrap();
        let handler = &groups[0]["hooks"][0];
        assert_eq!(handler["type"], "command");
        assert_eq!(handler["timeout"], CLAUDE_HOOK_TIMEOUT_SECS);
        assert!(handler["command"]
            .as_str()
            .unwrap()
            .contains("__hook-forward --source claude"));
    }

    #[test]
    fn remove_claude_preserves_user_hooks() {
        let existing = r#"{
          "hooks": {
            "PreToolUse": [
              {
                "matcher": "Bash",
                "hooks": [
                  { "type": "command", "command": "block-rm.sh" },
                  { "type": "command", "command": "/bin/mzp __hook-forward --source claude", "timeout": 5 }
                ]
              }
            ]
          }
        }"#;
        let (out, changed) = remove_claude_hooks(existing).unwrap();
        assert!(changed);
        assert!(out.contains("block-rm.sh"));
        assert!(!out.contains("__hook-forward --source claude"));
    }

    #[test]
    fn claude_events_exclude_worktree_create_and_file_changed() {
        assert!(!CLAUDE_EVENTS.contains(&"WorktreeCreate"));
        assert!(!CLAUDE_EVENTS.contains(&"FileChanged"));
        assert!(!CLAUDE_EVENTS.contains(&"MessageDisplay"));
        assert!(CLAUDE_EVENTS.contains(&"WorktreeRemove"));
    }

    #[test]
    fn merge_claude_invalid_json_errors() {
        let err = merge_claude_hooks("{", "/bin/mzp __hook-forward --source claude").unwrap_err();
        assert!(err.contains("invalid Claude settings.json"));
    }

    #[test]
    fn merge_claude_refreshes_existing_handler() {
        let cmd = managed_command(Path::new("/new/mzp"), HookSource::Claude);
        let existing = r#"{
          "hooks": {
            "Stop": [{ "hooks": [{ "type": "command", "command": "/old/mzp __hook-forward --source claude" }] }]
          }
        }"#;
        let (out, changed) = merge_claude_hooks(existing, &cmd).unwrap();
        assert!(changed);
        assert!(out.contains("/new/mzp"));
    }

    #[test]
    fn remove_claude_empty_hooks() {
        let (out, changed) = remove_claude_hooks("").unwrap();
        assert!(!changed);
        assert!(out.is_empty());
    }

    #[test]
    fn merge_claude_root_not_object_errors() {
        let err = merge_claude_hooks("[]", "cmd").unwrap_err();
        assert!(err.contains("root must be an object"));
    }

    #[test]
    fn merge_claude_event_not_array_errors() {
        let err = merge_claude_hooks(r#"{"hooks":{"Stop":{}}}"#, "cmd").unwrap_err();
        assert!(err.contains("must be an array"));
    }

    #[test]
    fn merge_claude_no_change_when_command_current() {
        let cmd = managed_command(Path::new("/bin/mzp"), HookSource::Claude);
        let (once, _) = merge_claude_hooks("", &cmd).unwrap();
        let (_, changed) = merge_claude_hooks(&once, &cmd).unwrap();
        assert!(!changed);
    }

    #[test]
    fn merge_claude_hooks_field_not_object_errors() {
        let err = merge_claude_hooks(r#"{"hooks":"nope"}"#, "cmd").unwrap_err();
        assert!(err.contains("`hooks` must be an object"));
    }

    #[test]
    fn remove_claude_invalid_json_errors() {
        assert!(remove_claude_hooks("not-json").is_err());
    }
}
