//! Cursor / Claude Code lifecycle hooks → Mizpah hub ingest.
//!
//! `mzp attach cursor|claude` merges observe-only command hooks into user-global
//! configs. Each hook invokes `mzp __hook-forward`, which POSTs a structured
//! envelope to `/api/ingest` and always exits 0 with empty stdout.

use crate::mcp;
use crate::mzp_meta::MzpMeta;
use crate::shell_attach::{self, DEFAULT_HOST, DEFAULT_PORT};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::debug;

const STATE_FILE: &str = "agent-hooks.json";
const HOOK_MARKER: &str = "__hook-forward --source ";
const MAX_STRING_BYTES: usize = 64 * 1024;
const FORWARD_HTTP_TIMEOUT: Duration = Duration::from_secs(2);
const CLAUDE_HOOK_TIMEOUT_SECS: u64 = 5;

const CURSOR_EVENTS: &[&str] = &[
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

const CLAUDE_EVENTS: &[&str] = &[
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookSource {
    Cursor,
    Claude,
}

impl HookSource {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cursor" => Some(Self::Cursor),
            "claude" => Some(Self::Claude),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cursor => "cursor",
            Self::Claude => "claude",
        }
    }

    fn default_service(self) -> &'static str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourceState {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub service: String,
}

impl SourceState {
    fn disabled(source: HookSource) -> Self {
        Self {
            enabled: false,
            host: DEFAULT_HOST.to_string(),
            port: DEFAULT_PORT,
            service: source.default_service().to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentHooksState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<SourceState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<SourceState>,
}

fn state_path() -> io::Result<PathBuf> {
    Ok(shell_attach::config_dir()?.join(STATE_FILE))
}

pub fn load_state() -> io::Result<AgentHooksState> {
    let path = state_path()?;
    match fs::read_to_string(&path) {
        Ok(raw) if raw.trim().is_empty() => Ok(AgentHooksState::default()),
        Ok(raw) => {
            serde_json::from_str(&raw).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(AgentHooksState::default()),
        Err(e) => Err(e),
    }
}

fn save_state(state: &AgentHooksState) -> io::Result<()> {
    let dir = shell_attach::config_dir()?;
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    let path = state_path()?;
    let raw = serde_json::to_string_pretty(state)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    atomic_write(&path, &raw)?;
    Ok(())
}

fn atomic_write(path: &Path, content: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path)
            .ok()
            .map(|m| m.permissions().mode())
            .unwrap_or(0o600);
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(mode));
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

fn home_dir() -> Option<PathBuf> {
    directories::UserDirs::new().map(|u| u.home_dir().to_path_buf())
}

pub fn cursor_hooks_path() -> Option<PathBuf> {
    Some(home_dir()?.join(".cursor").join("hooks.json"))
}

pub fn claude_settings_path() -> Option<PathBuf> {
    Some(home_dir()?.join(".claude").join("settings.json"))
}

fn shell_quote(path: &Path) -> String {
    let s = path.display().to_string();
    if s.is_empty() {
        return "''".to_string();
    }
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+@%=,:".contains(c))
    {
        return s;
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn managed_command(bin: &Path, source: HookSource) -> String {
    format!("{} {}{}", shell_quote(bin), HOOK_MARKER, source.as_str())
}

pub fn is_managed_command(command: &str, source: HookSource) -> bool {
    let needle = format!("{HOOK_MARKER}{}", source.as_str());
    command.contains(&needle)
}

/// Merge Mizpah handlers into a Cursor `hooks.json` document.
pub fn merge_cursor_hooks(existing: &str, command: &str) -> Result<(String, bool), String> {
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
pub fn remove_cursor_hooks(existing: &str) -> Result<(String, bool), String> {
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

/// Merge Mizpah handlers into a Claude Code `settings.json` document.
pub fn merge_claude_hooks(existing: &str, command: &str) -> Result<(String, bool), String> {
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
pub fn remove_claude_hooks(existing: &str) -> Result<(String, bool), String> {
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
                .map(|a| !a.is_empty())
                .unwrap_or(true)
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

fn read_file_or_empty(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e),
    }
}

fn write_config_file(path: &Path, content: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    atomic_write(path, content)
}

pub async fn run_attach_cursor(
    service: Option<String>,
    host: String,
    port: u16,
) -> Result<(), String> {
    run_attach_source(HookSource::Cursor, service, host, port).await
}

pub async fn run_attach_claude(
    service: Option<String>,
    host: String,
    port: u16,
) -> Result<(), String> {
    run_attach_source(HookSource::Claude, service, host, port).await
}

async fn run_attach_source(
    source: HookSource,
    service: Option<String>,
    host: String,
    port: u16,
) -> Result<(), String> {
    let bin = mcp::resolve_binary_path().map_err(|e| format!("could not resolve binary: {e}"))?;
    let command = managed_command(&bin, source);
    let service = service
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| source.default_service().to_string());

    let (path, merged) = match source {
        HookSource::Cursor => {
            let path = cursor_hooks_path()
                .ok_or_else(|| "could not resolve home directory for Cursor hooks".to_string())?;
            let existing = read_file_or_empty(&path).map_err(|e| e.to_string())?;
            let (out, changed) = merge_cursor_hooks(&existing, &command)?;
            if changed || existing.trim().is_empty() {
                write_config_file(&path, &out).map_err(|e| e.to_string())?;
            }
            (path, changed || existing.trim().is_empty())
        }
        HookSource::Claude => {
            let path = claude_settings_path().ok_or_else(|| {
                "could not resolve home directory for Claude settings".to_string()
            })?;
            let existing = read_file_or_empty(&path).map_err(|e| e.to_string())?;
            let (out, changed) = merge_claude_hooks(&existing, &command)?;
            if changed || existing.trim().is_empty() {
                write_config_file(&path, &out).map_err(|e| e.to_string())?;
            }
            (path, changed || existing.trim().is_empty())
        }
    };

    shell_attach::ensure_hub(&host, port, None).await?;

    let mut state = load_state().unwrap_or_default();
    let src_state = SourceState {
        enabled: true,
        host: host.clone(),
        port,
        service: service.clone(),
    };
    match source {
        HookSource::Cursor => state.cursor = Some(src_state),
        HookSource::Claude => state.claude = Some(src_state),
    }
    save_state(&state).map_err(|e| format!("failed to save agent-hooks state: {e}"))?;

    let url = shell_attach::hub_url(&host, port);
    eprintln!("mizpah: attached {} hooks → {url}", source.as_str());
    eprintln!("  config: {}", path.display());
    eprintln!("  service: {service}");
    if merged {
        eprintln!("  hooks: installed/updated");
    } else {
        eprintln!("  hooks: already present");
    }
    eprintln!("  note: re-run attach after moving the mzp binary so hook paths stay valid");
    Ok(())
}

pub fn run_detach_cursor() -> Result<(), String> {
    run_detach_source(HookSource::Cursor)
}

pub fn run_detach_claude() -> Result<(), String> {
    run_detach_source(HookSource::Claude)
}

fn run_detach_source(source: HookSource) -> Result<(), String> {
    let path = match source {
        HookSource::Cursor => cursor_hooks_path()
            .ok_or_else(|| "could not resolve home directory for Cursor hooks".to_string())?,
        HookSource::Claude => claude_settings_path()
            .ok_or_else(|| "could not resolve home directory for Claude settings".to_string())?,
    };

    let existing = read_file_or_empty(&path).map_err(|e| e.to_string())?;
    if !existing.trim().is_empty() {
        let (out, changed) = match source {
            HookSource::Cursor => remove_cursor_hooks(&existing)?,
            HookSource::Claude => remove_claude_hooks(&existing)?,
        };
        if changed {
            if out.trim().is_empty() || out.trim() == "{}" {
                // Keep a minimal valid file rather than deleting user config dirs.
                write_config_file(
                    &path,
                    if source == HookSource::Cursor {
                        "{\n  \"version\": 1,\n  \"hooks\": {}\n}\n"
                    } else {
                        "{}\n"
                    },
                )
                .map_err(|e| e.to_string())?;
            } else {
                write_config_file(&path, &out).map_err(|e| e.to_string())?;
            }
            eprintln!(
                "mizpah: detached {} hooks from {}",
                source.as_str(),
                path.display()
            );
        } else {
            eprintln!(
                "mizpah: no {} hooks found in {}",
                source.as_str(),
                path.display()
            );
        }
    } else {
        eprintln!(
            "mizpah: no {} config at {}",
            source.as_str(),
            path.display()
        );
    }

    let mut state = load_state().unwrap_or_default();
    match source {
        HookSource::Cursor => {
            if let Some(s) = state.cursor.as_mut() {
                s.enabled = false;
            } else {
                state.cursor = Some(SourceState::disabled(source));
            }
        }
        HookSource::Claude => {
            if let Some(s) = state.claude.as_mut() {
                s.enabled = false;
            } else {
                state.claude = Some(SourceState::disabled(source));
            }
        }
    }
    let _ = save_state(&state);
    Ok(())
}

pub fn run_detach_all() -> Result<(), String> {
    shell_attach::run_detach()?;
    run_detach_cursor()?;
    run_detach_claude()?;
    Ok(())
}

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

    let url = format!("{}/api/ingest", shell_attach::hub_url(&src.host, src.port));
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
pub fn build_envelope(source: HookSource, mut event: Value) -> Value {
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
    fn managed_command_contains_marker() {
        let cmd = managed_command(Path::new("/usr/local/bin/mzp"), HookSource::Cursor);
        assert!(is_managed_command(&cmd, HookSource::Cursor));
        assert!(!is_managed_command(&cmd, HookSource::Claude));
        assert!(cmd.contains("__hook-forward --source cursor"));
    }

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

    #[test]
    fn cursor_events_exclude_tab() {
        assert!(!CURSOR_EVENTS.contains(&"beforeTabFileRead"));
        assert!(!CURSOR_EVENTS.contains(&"afterTabFileEdit"));
    }

    #[test]
    fn claude_events_exclude_worktree_create_and_file_changed() {
        assert!(!CLAUDE_EVENTS.contains(&"WorktreeCreate"));
        assert!(!CLAUDE_EVENTS.contains(&"FileChanged"));
        assert!(!CLAUDE_EVENTS.contains(&"MessageDisplay"));
        assert!(CLAUDE_EVENTS.contains(&"WorktreeRemove"));
    }
}
