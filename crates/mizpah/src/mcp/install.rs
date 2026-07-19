//! Merge / remove Mizpah MCP server entries in Cursor, Claude, and Codex configs.

use serde_json::{json, Map, Value};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Value as TomlValue};

pub const SERVER_NAME: &str = "mizpah";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    Cursor,
    ClaudeDesktop,
    ClaudeCode,
    Codex,
}

impl ClientKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cursor => "Cursor",
            Self::ClaudeDesktop => "Claude Desktop",
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallAction {
    Updated,
    Removed,
    SkippedMissingProduct,
    Unchanged,
}

#[derive(Debug, Clone)]
pub struct InstallResult {
    pub client: ClientKind,
    pub path: PathBuf,
    pub action: InstallAction,
}

#[derive(Debug)]
pub struct InstallError {
    pub client: ClientKind,
    pub path: PathBuf,
    pub error: String,
}

pub struct InstallReport {
    pub results: Vec<InstallResult>,
    pub errors: Vec<InstallError>,
}

impl InstallReport {
    pub fn print_summary(&self) {
        for r in &self.results {
            let status = match r.action {
                InstallAction::Updated => "updated",
                InstallAction::Removed => "removed",
                InstallAction::SkippedMissingProduct => "skipped (product not found)",
                InstallAction::Unchanged => "unchanged",
            };
            eprintln!("  {}: {} ({})", r.client.label(), status, r.path.display());
        }
        for e in &self.errors {
            eprintln!(
                "  {}: error — {} ({})",
                e.client.label(),
                e.error,
                e.path.display()
            );
        }
    }
}

/// Absolute path to this binary for MCP `command` entries.
pub fn resolve_binary_path() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    match fs::canonicalize(&exe) {
        Ok(canon) => Ok(canon),
        Err(_) => Ok(exe),
    }
}

pub fn install_all(command: &Path) -> InstallReport {
    apply_all(command, false)
}

pub fn uninstall_all() -> InstallReport {
    apply_all(Path::new(""), true)
}

fn apply_all(command: &Path, uninstall: bool) -> InstallReport {
    let mut report = InstallReport {
        results: Vec::new(),
        errors: Vec::new(),
    };

    for (client, path_opt) in discover_clients() {
        let Some(path) = path_opt else {
            report.results.push(InstallResult {
                client,
                path: PathBuf::new(),
                action: InstallAction::SkippedMissingProduct,
            });
            continue;
        };

        let result = if uninstall {
            match client {
                ClientKind::Codex => uninstall_toml_mcp(&path),
                _ => uninstall_json_mcp(&path),
            }
        } else {
            match client {
                ClientKind::Codex => install_toml_mcp(&path, command),
                _ => install_json_mcp(&path, command),
            }
        };

        match result {
            Ok(action) => report.results.push(InstallResult {
                client,
                path,
                action,
            }),
            Err(error) => report.errors.push(InstallError {
                client,
                path,
                error,
            }),
        }
    }

    report
}

fn discover_clients() -> Vec<(ClientKind, Option<PathBuf>)> {
    vec![
        (ClientKind::Cursor, cursor_mcp_path()),
        (ClientKind::ClaudeDesktop, claude_desktop_config_path()),
        (ClientKind::ClaudeCode, claude_code_config_path()),
        (ClientKind::Codex, codex_config_path()),
    ]
}

fn cursor_mcp_path() -> Option<PathBuf> {
    let home = crate::util::home_dir()?;
    let dir = home.join(".cursor");
    if !dir.is_dir() {
        return None;
    }
    Some(dir.join("mcp.json"))
}

fn claude_desktop_config_path() -> Option<PathBuf> {
    let home = crate::util::home_dir()?;
    let dir = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Claude")
    } else if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)?
            .join("Claude")
    } else {
        home.join(".config/Claude")
    };
    if !dir.is_dir() {
        return None;
    }
    Some(dir.join("claude_desktop_config.json"))
}

fn claude_code_config_path() -> Option<PathBuf> {
    let home = crate::util::home_dir()?;
    let json = home.join(".claude.json");
    let dir = home.join(".claude");
    if json.is_file() || dir.is_dir() {
        Some(json)
    } else {
        None
    }
}

fn codex_config_path() -> Option<PathBuf> {
    let home = crate::util::home_dir()?;
    let dir = home.join(".codex");
    if !dir.is_dir() {
        return None;
    }
    Some(dir.join("config.toml"))
}

/// Pure merge helpers (also used in unit tests).
pub fn merge_json_mcp_servers(existing: &str, command: &str) -> Result<(String, bool), String> {
    let mut root: Value = if existing.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(existing).map_err(|e| format!("invalid JSON: {e}"))?
    };

    let obj = root
        .as_object_mut()
        .ok_or_else(|| "config root must be a JSON object".to_string())?;

    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()));
    let servers_obj = servers
        .as_object_mut()
        .ok_or_else(|| "mcpServers must be a JSON object".to_string())?;

    let entry = json!({
        "command": command,
        "args": ["mcp"]
    });

    let changed = servers_obj.get(SERVER_NAME) != Some(&entry);
    servers_obj.insert(SERVER_NAME.to_string(), entry);

    let out = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    Ok((format!("{out}\n"), changed))
}

pub fn remove_json_mcp_server(existing: &str) -> Result<(String, bool), String> {
    if existing.trim().is_empty() {
        return Ok((String::new(), false));
    }
    let mut root: Value =
        serde_json::from_str(existing).map_err(|e| format!("invalid JSON: {e}"))?;
    let obj = root
        .as_object_mut()
        .ok_or_else(|| "config root must be a JSON object".to_string())?;

    let changed = match obj.get_mut("mcpServers") {
        Some(Value::Object(servers)) => servers.remove(SERVER_NAME).is_some(),
        _ => false,
    };

    if let Some(Value::Object(servers)) = obj.get("mcpServers") {
        if servers.is_empty() {
            // Keep empty mcpServers object — safer for other tools that expect the key.
        }
    }

    let out = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    Ok((format!("{out}\n"), changed))
}

pub fn merge_toml_mcp_servers(existing: &str, command: &str) -> Result<(String, bool), String> {
    let mut doc: DocumentMut = if existing.trim().is_empty() {
        DocumentMut::new()
    } else {
        existing.parse().map_err(|e| format!("invalid TOML: {e}"))?
    };

    let servers = doc
        .entry("mcp_servers")
        .or_insert(toml_edit::table())
        .as_table_mut()
        .ok_or_else(|| "mcp_servers must be a table".to_string())?;

    let server = servers
        .entry(SERVER_NAME)
        .or_insert(toml_edit::table())
        .as_table_mut()
        .ok_or_else(|| "mcp_servers.mizpah must be a table".to_string())?;

    let mut changed = false;
    let current_cmd = server
        .get("command")
        .and_then(|i| i.as_value())
        .and_then(|v| v.as_str());
    if current_cmd != Some(command) {
        server.insert("command", Item::Value(TomlValue::from(command)));
        changed = true;
    }

    let args_match = server
        .get("args")
        .and_then(|i| i.as_value())
        .and_then(|v| v.as_array())
        .is_some_and(|arr| {
            arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>() == vec!["mcp"]
        });
    if !args_match {
        let desired_args = toml_edit::Array::from_iter(["mcp"]);
        server.insert("args", Item::Value(TomlValue::Array(desired_args)));
        changed = true;
    }

    Ok((doc.to_string(), changed))
}

pub fn remove_toml_mcp_server(existing: &str) -> Result<(String, bool), String> {
    if existing.trim().is_empty() {
        return Ok((String::new(), false));
    }
    let mut doc: DocumentMut = existing.parse().map_err(|e| format!("invalid TOML: {e}"))?;

    let changed = match doc.get_mut("mcp_servers").and_then(|i| i.as_table_mut()) {
        Some(servers) => servers.remove(SERVER_NAME).is_some(),
        None => false,
    };

    Ok((doc.to_string(), changed))
}

fn install_json_mcp(path: &Path, command: &Path) -> Result<InstallAction, String> {
    let existing = read_or_empty(path)?;
    let (next, changed) = merge_json_mcp_servers(&existing, &command.to_string_lossy())?;
    if !changed && path.is_file() {
        return Ok(InstallAction::Unchanged);
    }
    atomic_write(path, &next)?;
    Ok(InstallAction::Updated)
}

fn uninstall_json_mcp(path: &Path) -> Result<InstallAction, String> {
    if !path.is_file() {
        return Ok(InstallAction::Unchanged);
    }
    let existing = read_or_empty(path)?;
    let (next, changed) = remove_json_mcp_server(&existing)?;
    if !changed {
        return Ok(InstallAction::Unchanged);
    }
    atomic_write(path, &next)?;
    Ok(InstallAction::Removed)
}

fn install_toml_mcp(path: &Path, command: &Path) -> Result<InstallAction, String> {
    let existing = read_or_empty(path)?;
    let (next, changed) = merge_toml_mcp_servers(&existing, &command.to_string_lossy())?;
    if !changed && path.is_file() {
        return Ok(InstallAction::Unchanged);
    }
    atomic_write(path, &next)?;
    Ok(InstallAction::Updated)
}

fn uninstall_toml_mcp(path: &Path) -> Result<InstallAction, String> {
    if !path.is_file() {
        return Ok(InstallAction::Unchanged);
    }
    let existing = read_or_empty(path)?;
    let (next, changed) = remove_toml_mcp_server(&existing)?;
    if !changed {
        return Ok(InstallAction::Unchanged);
    }
    atomic_write(path, &next)?;
    Ok(InstallAction::Removed)
}

fn read_or_empty(path: &Path) -> Result<String, String> {
    if !path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(path).map_err(|e| format!("read failed: {e}"))
}

fn atomic_write(path: &Path, contents: &str) -> Result<(), String> {
    crate::util::atomic_write(path, contents).map_err(|e| e.to_string())
}

/// Best-effort upsert used on hub start (never fails the hub).
pub fn ensure_registered_on_hub_start() {
    let Ok(bin) = resolve_binary_path() else {
        return;
    };
    let report = install_all(&bin);
    let any_updated = report
        .results
        .iter()
        .any(|r| r.action == InstallAction::Updated);
    if any_updated {
        eprintln!(
            "mizpah: registered MCP server with local AI clients (restart them to pick up tools)"
        );
        report.print_summary();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_json_empty() {
        let (out, changed) = merge_json_mcp_servers("", "/bin/mizpah").unwrap();
        assert!(changed);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["mcpServers"]["mizpah"]["command"].as_str(),
            Some("/bin/mizpah")
        );
        assert_eq!(v["mcpServers"]["mizpah"]["args"], json!(["mcp"]));
    }

    #[test]
    fn merge_json_preserves_others() {
        let existing = r#"{
  "mcpServers": {
    "other": { "command": "npx", "args": ["x"] }
  }
}"#;
        let (out, changed) = merge_json_mcp_servers(existing, "/usr/bin/mizpah").unwrap();
        assert!(changed);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v["mcpServers"].get("other").is_some());
        assert_eq!(
            v["mcpServers"]["mizpah"]["command"].as_str(),
            Some("/usr/bin/mizpah")
        );
    }

    #[test]
    fn merge_json_idempotent() {
        let (once, _) = merge_json_mcp_servers("", "/bin/mizpah").unwrap();
        let (_, changed) = merge_json_mcp_servers(&once, "/bin/mizpah").unwrap();
        assert!(!changed);
    }

    #[test]
    fn remove_json_server() {
        let existing =
            r#"{"mcpServers":{"mizpah":{"command":"x","args":["mcp"]},"other":{"command":"y"}}}"#;
        let (out, changed) = remove_json_mcp_server(existing).unwrap();
        assert!(changed);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v["mcpServers"].get("mizpah").is_none());
        assert!(v["mcpServers"].get("other").is_some());
    }

    #[test]
    fn merge_toml_empty() {
        let (out, changed) = merge_toml_mcp_servers("", "/bin/mizpah").unwrap();
        assert!(changed);
        assert!(out.contains("[mcp_servers.mizpah]"));
        assert!(out.contains("command = \"/bin/mizpah\""));
        assert!(out.contains("args = [\"mcp\"]"));
    }

    #[test]
    fn merge_toml_preserves_others() {
        let existing = r#"
model = "gpt"

[mcp_servers.github]
command = "npx"
"#;
        let (out, changed) = merge_toml_mcp_servers(existing, "/opt/mizpah").unwrap();
        assert!(changed);
        assert!(out.contains("model = \"gpt\""));
        assert!(out.contains("[mcp_servers.github]"));
        assert!(out.contains("[mcp_servers.mizpah]"));
    }

    #[test]
    fn merge_toml_idempotent() {
        let (once, _) = merge_toml_mcp_servers("", "/bin/mizpah").unwrap();
        let (_, changed) = merge_toml_mcp_servers(&once, "/bin/mizpah").unwrap();
        assert!(!changed);
    }

    #[test]
    fn remove_toml_server() {
        let existing = r#"
[mcp_servers.mizpah]
command = "/bin/mizpah"
args = ["mcp"]

[mcp_servers.other]
command = "npx"
"#;
        let (out, changed) = remove_toml_mcp_server(existing).unwrap();
        assert!(changed);
        assert!(!out.contains("mizpah"));
        assert!(out.contains("[mcp_servers.other]"));
    }

    #[test]
    fn merge_json_updates_changed_command() {
        let existing = r#"{"mcpServers":{"mizpah":{"command":"/old/path","args":["mcp"]}}}"#;
        let (out, changed) = merge_json_mcp_servers(existing, "/new/path").unwrap();
        assert!(changed);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["mcpServers"]["mizpah"]["command"].as_str(),
            Some("/new/path")
        );
    }

    #[test]
    fn merge_toml_updates_changed_command() {
        let existing = r#"
[mcp_servers.mizpah]
command = "/old/path"
args = ["mcp"]
"#;
        let (out, changed) = merge_toml_mcp_servers(existing, "/new/path").unwrap();
        assert!(changed);
        assert!(out.contains("/new/path"));
        assert!(!out.contains("/old/path"));
    }

    #[test]
    fn remove_json_empty_input() {
        let (out, changed) = remove_json_mcp_server("").unwrap();
        assert!(!changed);
        assert_eq!(out, "");
    }

    #[test]
    fn remove_toml_empty_input() {
        let (out, changed) = remove_toml_mcp_server("").unwrap();
        assert!(!changed);
        assert_eq!(out, "");
    }

    #[test]
    fn remove_json_nonexistent_server() {
        let existing = r#"{"mcpServers":{"other":{"command":"x"}}}"#;
        let (out, changed) = remove_json_mcp_server(existing).unwrap();
        assert!(!changed);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v["mcpServers"].get("other").is_some());
    }

    #[test]
    fn remove_toml_nonexistent_server() {
        let existing = r#"
[mcp_servers.other]
command = "npx"
"#;
        let (out, changed) = remove_toml_mcp_server(existing).unwrap();
        assert!(!changed);
        assert!(out.contains("[mcp_servers.other]"));
    }

    #[test]
    fn merge_json_invalid_root_type() {
        let existing = r#"["array", "not", "object"]"#;
        let err = merge_json_mcp_servers(existing, "/bin/mizpah").unwrap_err();
        assert!(err.contains("must be a JSON object"));
    }

    #[test]
    fn merge_json_invalid_mcpservers_type() {
        let existing = r#"{"mcpServers": "not-an-object"}"#;
        let err = merge_json_mcp_servers(existing, "/bin/mizpah").unwrap_err();
        assert!(err.contains("must be a JSON object"));
    }

    #[test]
    fn merge_toml_invalid_syntax() {
        let existing = r#"this is [not valid toml"#;
        let err = merge_toml_mcp_servers(existing, "/bin/mizpah").unwrap_err();
        assert!(err.contains("invalid TOML"));
    }

    #[test]
    fn remove_json_invalid_syntax() {
        let existing = r#"{"invalid": json}"#;
        let err = remove_json_mcp_server(existing).unwrap_err();
        assert!(err.contains("invalid JSON"));
    }

    #[test]
    fn remove_toml_invalid_syntax() {
        let existing = r#"[invalid toml"#;
        let err = remove_toml_mcp_server(existing).unwrap_err();
        assert!(err.contains("invalid TOML"));
    }

    #[test]
    fn merge_json_creates_mcpservers_if_missing() {
        let existing = r#"{"other": "field"}"#;
        let (out, changed) = merge_json_mcp_servers(existing, "/bin/mizpah").unwrap();
        assert!(changed);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("mcpServers").is_some());
        assert!(v.get("other").is_some());
    }

    #[test]
    fn client_kind_labels() {
        assert_eq!(ClientKind::Cursor.label(), "Cursor");
        assert_eq!(ClientKind::ClaudeDesktop.label(), "Claude Desktop");
        assert_eq!(ClientKind::ClaudeCode.label(), "Claude Code");
        assert_eq!(ClientKind::Codex.label(), "Codex");
    }

    #[test]
    fn install_action_variants() {
        assert_eq!(InstallAction::Updated, InstallAction::Updated);
        assert_ne!(InstallAction::Updated, InstallAction::Removed);
    }

    fn with_isolated_home<F: FnOnce(&Path)>(f: F) {
        let _guard = crate::test_support::env_lock();
        let home = tempfile::tempdir().expect("tempdir");
        let old = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());
        f(home.path());
        match old {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn resolve_binary_path_ok() {
        let path = resolve_binary_path().expect("current exe");
        assert!(path.is_absolute() || path.exists());
    }

    #[test]
    fn install_report_print_summary_smoke() {
        let report = InstallReport {
            results: vec![
                InstallResult {
                    client: ClientKind::Cursor,
                    path: PathBuf::from("/tmp/mcp.json"),
                    action: InstallAction::Updated,
                },
                InstallResult {
                    client: ClientKind::ClaudeDesktop,
                    path: PathBuf::new(),
                    action: InstallAction::SkippedMissingProduct,
                },
                InstallResult {
                    client: ClientKind::ClaudeCode,
                    path: PathBuf::from("/tmp/.claude.json"),
                    action: InstallAction::Unchanged,
                },
                InstallResult {
                    client: ClientKind::Codex,
                    path: PathBuf::from("/tmp/config.toml"),
                    action: InstallAction::Removed,
                },
            ],
            errors: vec![InstallError {
                client: ClientKind::Codex,
                path: PathBuf::from("/tmp/config.toml"),
                error: "read failed".into(),
            }],
        };
        report.print_summary();
    }

    #[test]
    #[cfg(not(miri))]
    fn install_all_skips_when_product_dirs_missing() {
        with_isolated_home(|_home| {
            let report = install_all(Path::new("/opt/mizpah/bin"));
            assert!(report.errors.is_empty());
            assert!(report.results.iter().all(|r| {
                matches!(r.action, InstallAction::SkippedMissingProduct)
            }));
        });
    }

    #[test]
    #[cfg(not(miri))]
    fn install_and_uninstall_cursor_json() {
        with_isolated_home(|home| {
            fs::create_dir_all(home.join(".cursor")).unwrap();
            let install = install_all(Path::new("/opt/mizpah/bin"));
            let cursor = install
                .results
                .iter()
                .find(|r| r.client == ClientKind::Cursor)
                .expect("cursor result");
            assert_eq!(cursor.action, InstallAction::Updated);
            let content = fs::read_to_string(home.join(".cursor/mcp.json")).unwrap();
            assert!(content.contains("mizpah"));

            let second = install_all(Path::new("/opt/mizpah/bin"));
            let unchanged = second
                .results
                .iter()
                .find(|r| r.client == ClientKind::Cursor)
                .expect("cursor result");
            assert_eq!(unchanged.action, InstallAction::Unchanged);

            let uninstall = uninstall_all();
            let removed = uninstall
                .results
                .iter()
                .find(|r| r.client == ClientKind::Cursor)
                .expect("cursor result");
            assert_eq!(removed.action, InstallAction::Removed);
        });
    }

    #[test]
    #[cfg(not(miri))]
    fn install_and_uninstall_codex_toml() {
        with_isolated_home(|home| {
            fs::create_dir_all(home.join(".codex")).unwrap();
            install_all(Path::new("/usr/local/bin/mizpah"));
            let path = home.join(".codex/config.toml");
            assert!(path.is_file());
            let content = fs::read_to_string(&path).unwrap();
            assert!(content.contains("[mcp_servers.mizpah]"));

            uninstall_all();
            let after = fs::read_to_string(&path).unwrap();
            assert!(!after.contains("[mcp_servers.mizpah]"));
        });
    }

    #[test]
    #[cfg(not(miri))]
    fn ensure_registered_on_hub_start_smoke() {
        with_isolated_home(|home| {
            fs::create_dir_all(home.join(".cursor")).unwrap();
            ensure_registered_on_hub_start();
            assert!(home.join(".cursor/mcp.json").is_file());
        });
    }

    #[test]
    #[cfg(all(not(miri), target_os = "macos"))]
    fn install_claude_desktop_json() {
        with_isolated_home(|home| {
            fs::create_dir_all(home.join("Library/Application Support/Claude")).unwrap();
            let report = install_all(Path::new("/Applications/mizpah"));
            let desktop = report
                .results
                .iter()
                .find(|r| r.client == ClientKind::ClaudeDesktop)
                .expect("claude desktop result");
            assert_eq!(desktop.action, InstallAction::Updated);
            let content =
                fs::read_to_string(home.join("Library/Application Support/Claude/claude_desktop_config.json"))
                    .unwrap();
            assert!(content.contains("mizpah"));
        });
    }

    #[test]
    #[cfg(all(not(miri), unix))]
    fn install_json_read_error_surfaces_in_report() {
        with_isolated_home(|home| {
            fs::create_dir_all(home.join(".cursor")).unwrap();
            let path = home.join(".cursor/mcp.json");
            fs::write(&path, "{").unwrap();
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
            let report = install_all(Path::new("/bin/mizpah"));
            assert!(report
                .errors
                .iter()
                .any(|e| e.client == ClientKind::Cursor && e.error.contains("read failed")));
        });
    }

    #[test]
    #[cfg(not(miri))]
    fn install_and_uninstall_claude_code_json() {
        with_isolated_home(|home| {
            fs::write(home.join(".claude.json"), "{}").unwrap();
            let report = install_all(Path::new("/opt/mizpah/bin"));
            let claude = report
                .results
                .iter()
                .find(|r| r.client == ClientKind::ClaudeCode)
                .expect("claude code result");
            assert_eq!(claude.action, InstallAction::Updated);
            let content = fs::read_to_string(home.join(".claude.json")).unwrap();
            assert!(content.contains("mizpah"));
            uninstall_all();
            let after = fs::read_to_string(home.join(".claude.json")).unwrap();
            assert!(!after.contains("\"mizpah\""));
        });
    }

    #[test]
    fn merge_toml_invalid_mcp_servers_type() {
        let existing = r#"mcp_servers = "not-a-table""#;
        let err = merge_toml_mcp_servers(existing, "/bin/mizpah").unwrap_err();
        assert!(err.contains("must be a table"));
    }

    #[test]
    fn uninstall_json_missing_file_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.json");
        let action = uninstall_json_mcp(&path).unwrap();
        assert_eq!(action, InstallAction::Unchanged);
    }
}
