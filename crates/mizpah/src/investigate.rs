//! Launch local Claude Code / Cursor Agent sessions from a log entry.

use crate::store::LogEntry;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InvestigateTarget {
    Claude,
    Cursor,
}

impl InvestigateTarget {
    pub fn cli_names(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["claude"],
            Self::Cursor => &["agent", "cursor-agent"],
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Cursor => "Cursor Agent",
        }
    }
}

pub fn build_prompt(entry: &LogEntry) -> String {
    let data = match serde_json::to_string_pretty(&entry.data) {
        Ok(s) => s,
        Err(_) => entry.data.to_string(),
    };
    format!(
        r#"Investigate this log entry from Mizpah (live JSON log hub).

Service: {service}
Received at: {received_at}
Log id: {id}

Log entry:
```json
{data}
```

Use the Mizpah MCP tools to pull surrounding context from the live hub:
1. Call get_logs_around with id={id} (and service="{service}" if helpful).
2. Use search_logs with a CEL filter if you need related errors (keep limits small).

Then find the root cause in this codebase and propose a fix."#,
        service = entry.service,
        received_at = entry.received_at.to_rfc3339(),
        id = entry.id,
        data = data,
    )
}

pub fn find_cli(target: InvestigateTarget) -> Result<PathBuf, String> {
    find_on_path(target.cli_names()).ok_or_else(|| {
        format!(
            "{} CLI not found on PATH (tried: {})",
            target.display_name(),
            target.cli_names().join(", ")
        )
    })
}

fn find_on_path(names: &[&str]) -> Option<PathBuf> {
    for name in names {
        if let Some(p) = crate::util::which(name) {
            return Some(p);
        }
        #[cfg(windows)]
        {
            if let Some(p) = crate::util::which(&format!("{name}.exe")) {
                return Some(p);
            }
        }
    }
    None
}

/// Write prompt + launcher script, then open a new terminal running the agent.
pub fn launch_session(
    target: InvestigateTarget,
    entry: &LogEntry,
    project_dir: &Path,
) -> Result<(), String> {
    let cli = find_cli(target)?;
    let project = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());
    if !project.is_dir() {
        return Err(format!(
            "project directory does not exist: {}",
            project.display()
        ));
    }

    let prompt = build_prompt(entry);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let tmp = std::env::temp_dir();
    let prompt_path = tmp.join(format!("mizpah-investigate-{stamp}.txt"));

    fs::write(&prompt_path, prompt.as_bytes())
        .map_err(|e| format!("failed to write prompt file: {e}"))?;

    let launcher_path = write_launcher(&tmp, stamp, &project, &cli, &prompt_path)?;
    open_terminal(&launcher_path)?;
    Ok(())
}

#[cfg(not(windows))]
fn write_launcher(
    tmp: &Path,
    stamp: u128,
    project: &Path,
    cli: &Path,
    prompt_path: &Path,
) -> Result<PathBuf, String> {
    let launcher_path = tmp.join(format!("mizpah-investigate-{stamp}.sh"));
    let script = format!(
        "#!/bin/sh\n\
         set -e\n\
         cd {project}\n\
         PROMPT=$(cat {prompt})\n\
         exec {cli} \"$PROMPT\"\n",
        project = sh_single_quote(&project.display().to_string()),
        prompt = sh_single_quote(&prompt_path.display().to_string()),
        cli = sh_single_quote(&cli.display().to_string()),
    );
    {
        let mut f = fs::File::create(&launcher_path)
            .map_err(|e| format!("failed to write launcher: {e}"))?;
        f.write_all(script.as_bytes())
            .map_err(|e| format!("failed to write launcher: {e}"))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&launcher_path)
            .map_err(|e| format!("failed to stat launcher: {e}"))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&launcher_path, perms)
            .map_err(|e| format!("failed to chmod launcher: {e}"))?;
    }
    Ok(launcher_path)
}

#[cfg(windows)]
fn write_launcher(
    tmp: &Path,
    stamp: u128,
    project: &Path,
    cli: &Path,
    prompt_path: &Path,
) -> Result<PathBuf, String> {
    let launcher_path = tmp.join(format!("mizpah-investigate-{stamp}.ps1"));
    let script = format!(
        "$ErrorActionPreference = 'Stop'\n\
         Set-Location -LiteralPath {project}\n\
         $prompt = Get-Content -LiteralPath {prompt} -Raw\n\
         & {cli} $prompt\n",
        project = powershell_single_quote(&project.display().to_string()),
        prompt = powershell_single_quote(&prompt_path.display().to_string()),
        cli = powershell_single_quote(&cli.display().to_string()),
    );
    fs::write(&launcher_path, script).map_err(|e| format!("failed to write launcher: {e}"))?;
    Ok(launcher_path)
}

fn sh_single_quote(s: &str) -> String {
    crate::util::shell_single_quote(s)
}

#[cfg(windows)]
fn powershell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn open_terminal(launcher: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        open_terminal_macos(launcher)
    }
    #[cfg(target_os = "linux")]
    {
        open_terminal_linux(launcher)
    }
    #[cfg(windows)]
    {
        open_terminal_windows(launcher)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = launcher;
        Err("opening a terminal is not supported on this platform".into())
    }
}

#[cfg(target_os = "macos")]
fn open_terminal_macos(launcher: &Path) -> Result<(), String> {
    let path = launcher
        .canonicalize()
        .unwrap_or_else(|_| launcher.to_path_buf());
    let quoted = sh_single_quote(&path.display().to_string());
    // AppleScript string: escape backslash and double-quote.
    let do_script = format!("exec {quoted}");
    let escaped = do_script.replace('\\', "\\\\").replace('"', "\\\"");
    let source = format!(
        r#"tell application "Terminal"
    activate
    do script "{escaped}"
end tell"#
    );
    let status = Command::new("osascript")
        .arg("-e")
        .arg(&source)
        .status()
        .map_err(|e| format!("failed to open Terminal: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("osascript exited with {status}"))
    }
}

#[cfg(target_os = "linux")]
fn open_terminal_linux(launcher: &Path) -> Result<(), String> {
    let path = launcher
        .canonicalize()
        .unwrap_or_else(|_| launcher.to_path_buf());

    if let Ok(term) = std::env::var("TERMINAL") {
        if !term.is_empty() {
            if Command::new(&term).arg("-e").arg(&path).spawn().is_ok() {
                return Ok(());
            }
            if Command::new(&term).arg(&path).spawn().is_ok() {
                return Ok(());
            }
        }
    }

    let candidates: &[(&str, &[&str])] = &[
        ("gnome-terminal", &["--"]),
        ("kgx", &["--"]),
        ("konsole", &["-e"]),
        ("xfce4-terminal", &["-e"]),
        ("kitty", &[]),
        ("alacritty", &["-e"]),
        ("xterm", &["-e"]),
        ("x-terminal-emulator", &["-e"]),
    ];

    for (bin, prefix) in candidates {
        if find_on_path(&[bin]).is_none() {
            continue;
        }
        let mut cmd = Command::new(bin);
        for arg in *prefix {
            cmd.arg(arg);
        }
        cmd.arg(&path);
        if cmd.spawn().is_ok() {
            return Ok(());
        }
    }

    Err("no terminal emulator found (set $TERMINAL or install gnome-terminal/kitty/xterm)".into())
}

#[cfg(windows)]
fn open_terminal_windows(launcher: &Path) -> Result<(), String> {
    let path = launcher
        .canonicalize()
        .unwrap_or_else(|_| launcher.to_path_buf());
    if Command::new("wt.exe")
        .args(["-w", "0", "nt", "powershell", "-NoExit", "-File"])
        .arg(&path)
        .spawn()
        .is_ok()
    {
        return Ok(());
    }
    let status = Command::new("cmd")
        .args(["/C", "start", "", "powershell", "-NoExit", "-File"])
        .arg(&path)
        .status()
        .map_err(|e| format!("failed to open terminal: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("failed to open terminal: {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    fn sample_entry() -> LogEntry {
        LogEntry {
            id: 42,
            received_at: Utc::now(),
            service: "api".into(),
            data: json!({"level": "error", "msg": "boom"}),
            approx_bytes: 0,
        }
    }

    #[test]
    fn prompt_includes_id_service_and_mcp_hint() {
        let p = build_prompt(&sample_entry());
        assert!(p.contains("Log id: 42"));
        assert!(p.contains("Service: api"));
        assert!(p.contains("get_logs_around"));
        assert!(p.contains("search_logs"));
        assert!(p.contains("\"msg\": \"boom\"") || p.contains("\"msg\":\"boom\""));
    }

    #[test]
    fn sh_single_quote_escapes_apostrophe() {
        assert_eq!(sh_single_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_single_quote("/tmp/foo"), "'/tmp/foo'");
    }

    #[test]
    fn target_cli_names() {
        assert_eq!(InvestigateTarget::Claude.cli_names(), &["claude"]);
        assert_eq!(
            InvestigateTarget::Cursor.cli_names(),
            &["agent", "cursor-agent"]
        );
    }
}
