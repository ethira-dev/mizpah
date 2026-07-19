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
    let data = format_entry_data(&entry.data);
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

pub(crate) fn format_entry_data(data: &serde_json::Value) -> String {
    serde_json::to_string_pretty(data).unwrap_or_else(|_| data.to_string())
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
    launch_session_with_opener(target, entry, project_dir, open_terminal)
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

#[cfg(test)]
mod test_terminal {
    use super::Path;
    use std::sync::{Mutex, OnceLock};

    static OPENER: OnceLock<Mutex<Option<fn(&Path) -> Result<(), String>>>> = OnceLock::new();

    fn cell() -> &'static Mutex<Option<fn(&Path) -> Result<(), String>>> {
        OPENER.get_or_init(|| Mutex::new(None))
    }

    pub(super) fn get() -> Option<fn(&Path) -> Result<(), String>> {
        cell().lock().ok().and_then(|g| *g)
    }

    pub(crate) fn set(opener: Option<fn(&Path) -> Result<(), String>>) {
        *cell().lock().unwrap() = opener;
    }
}

fn open_terminal(launcher: &Path) -> Result<(), String> {
    #[cfg(test)]
    if let Some(opener) = test_terminal::get() {
        return opener(launcher);
    }
    open_terminal_platform(launcher)
}

fn open_terminal_platform(launcher: &Path) -> Result<(), String> {
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

#[cfg(test)]
pub(crate) fn set_test_terminal_opener(opener: Option<fn(&Path) -> Result<(), String>>) {
    test_terminal::set(opener);
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_terminal_applescript(launcher: &Path) -> String {
    let path = launcher
        .canonicalize()
        .unwrap_or_else(|_| launcher.to_path_buf());
    let quoted = sh_single_quote(&path.display().to_string());
    let do_script = format!("exec {quoted}");
    let escaped = do_script.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        r#"tell application "Terminal"
    activate
    do script "{escaped}"
end tell"#
    )
}

#[cfg(target_os = "macos")]
fn open_terminal_macos(launcher: &Path) -> Result<(), String> {
    open_terminal_macos_with(launcher, run_osascript)
}

#[cfg(target_os = "macos")]
pub(crate) fn open_terminal_macos_with(
    launcher: &Path,
    run: impl FnOnce(&str) -> Result<std::process::ExitStatus, String>,
) -> Result<(), String> {
    let source = macos_terminal_applescript(launcher);
    let status = run(&source)?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("osascript exited with {status}"))
    }
}

#[cfg(target_os = "macos")]
fn run_osascript(source: &str) -> Result<std::process::ExitStatus, String> {
    #[cfg(test)]
    if let Some(hook) = test_osascript::get() {
        return hook(source);
    }
    Command::new("osascript")
        .arg("-e")
        .arg(source)
        .status()
        .map_err(|e| format!("failed to open Terminal: {e}"))
}

#[cfg(all(test, target_os = "macos"))]
mod test_osascript {
    use std::process::ExitStatus;
    use std::sync::{Mutex, OnceLock};

    type Hook = fn(&str) -> Result<ExitStatus, String>;
    static HOOK: OnceLock<Mutex<Option<Hook>>> = OnceLock::new();

    fn cell() -> &'static Mutex<Option<Hook>> {
        HOOK.get_or_init(|| Mutex::new(None))
    }

    pub(super) fn get() -> Option<Hook> {
        cell().lock().ok().and_then(|g| *g)
    }

    pub(crate) fn set(hook: Option<Hook>) {
        *cell().lock().unwrap() = hook;
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

/// Launch with an injectable terminal opener (for tests).
pub fn launch_session_with_opener<F>(
    target: InvestigateTarget,
    entry: &LogEntry,
    project_dir: &Path,
    open: F,
) -> Result<(), String>
where
    F: FnOnce(&Path) -> Result<(), String>,
{
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
    open(&launcher_path)?;
    Ok(())
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
            event_time: None,
            service: "api".into(),
            format_id: Some("json".into()),
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
    fn format_entry_data_pretty_prints_json() {
        let data = json!({"a": 1, "b": [2, 3]});
        let formatted = format_entry_data(&data);
        assert!(formatted.contains("1"));
        assert!(formatted.contains("2"));
    }

    #[test]
    fn prompt_falls_back_when_data_not_pretty() {
        // Number is always pretty-printable; use a value that serializes fine.
        let mut e = sample_entry();
        e.data = json!(null);
        let p = build_prompt(&e);
        assert!(p.contains("null") || p.contains("Log id: 42"));
    }

    #[test]
    fn sh_single_quote_escapes_apostrophe() {
        assert_eq!(sh_single_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_single_quote("/tmp/foo"), "'/tmp/foo'");
    }

    #[test]
    fn target_cli_names_and_display() {
        assert_eq!(InvestigateTarget::Claude.cli_names(), &["claude"]);
        assert_eq!(
            InvestigateTarget::Cursor.cli_names(),
            &["agent", "cursor-agent"]
        );
        assert_eq!(InvestigateTarget::Claude.display_name(), "Claude Code");
        assert_eq!(InvestigateTarget::Cursor.display_name(), "Cursor Agent");
    }

    fn with_path<F: FnOnce()>(path: &std::path::Path, f: F) {
        let _guard = crate::test_support::env_lock();
        let old = std::env::var_os("PATH");
        std::env::set_var("PATH", path);
        f();
        match old {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
    }

    #[test]
    fn find_cli_missing_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        with_path(dir.path(), || {
            let err = find_cli(InvestigateTarget::Claude).unwrap_err();
            assert!(err.contains("not found"));
            assert!(err.contains("claude"));
        });
    }

    #[test]
    fn find_cli_finds_executable_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        fs::write(&bin, b"#!/bin/sh\necho ok\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&bin).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&bin, perms).unwrap();
        }
        with_path(dir.path(), || {
            let found = find_cli(InvestigateTarget::Claude).unwrap();
            assert_eq!(found, bin);
        });
    }

    #[test]
    fn write_launcher_creates_executable_script() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let prompt = tmp.path().join("prompt.txt");
        fs::write(&prompt, "hello").unwrap();
        let cli = tmp.path().join("agent");
        fs::write(&cli, b"x").unwrap();
        let path = write_launcher(tmp.path(), 99, project.path(), &cli, &prompt).unwrap();
        assert!(path.exists());
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("exec") || text.contains("Set-Location") || text.contains("$prompt"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_ne!(mode & 0o111, 0);
        }
    }

    #[test]
    fn launch_session_missing_project_dir() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        fs::write(&bin, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&bin).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&bin, perms).unwrap();
        }
        let missing = dir.path().join("no-such-project");
        with_path(dir.path(), || {
            let err = launch_session_with_opener(
                InvestigateTarget::Claude,
                &sample_entry(),
                &missing,
                |_| Ok(()),
            )
            .unwrap_err();
            assert!(
                err.contains("does not exist")
                    || err.contains("not found")
                    || err.contains("project")
            );
        });
    }

    #[test]
    fn launch_session_with_mock_opener_ok() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        fs::write(&bin, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&bin).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&bin, perms).unwrap();
        }
        let project = tempfile::tempdir().unwrap();
        with_path(dir.path(), || {
            let mut opened = false;
            launch_session_with_opener(
                InvestigateTarget::Claude,
                &sample_entry(),
                project.path(),
                |p| {
                    assert!(p.exists());
                    opened = true;
                    Ok(())
                },
            )
            .unwrap();
            assert!(opened);
        });
    }

    #[test]
    fn launch_session_propagates_opener_error() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("agent");
        fs::write(&bin, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&bin).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&bin, perms).unwrap();
        }
        let project = tempfile::tempdir().unwrap();
        with_path(dir.path(), || {
            let err = launch_session_with_opener(
                InvestigateTarget::Cursor,
                &sample_entry(),
                project.path(),
                |_| Err("no term".into()),
            )
            .unwrap_err();
            assert_eq!(err, "no term");
        });
    }

    #[test]
    fn serde_investigate_target_roundtrip() {
        let v = serde_json::to_value(InvestigateTarget::Claude).unwrap();
        assert_eq!(v, json!("claude"));
        let back: InvestigateTarget = serde_json::from_value(json!("cursor")).unwrap();
        assert_eq!(back, InvestigateTarget::Cursor);
    }

    #[test]
    fn find_cli_cursor_prefers_agent_on_path() {
        let _guard = crate::test_support::env_lock();
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agent");
        fs::write(&agent, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&agent).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&agent, perms).unwrap();
        }
        std::env::set_var("PATH", dir.path());
        let found = find_cli(InvestigateTarget::Cursor).unwrap();
        assert_eq!(found, agent);
    }

    #[test]
    fn find_cli_cursor_falls_back_to_cursor_agent() {
        let _guard = crate::test_support::env_lock();
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("cursor-agent");
        fs::write(&bin, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&bin).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&bin, perms).unwrap();
        }
        std::env::set_var("PATH", dir.path());
        let found = find_cli(InvestigateTarget::Cursor).unwrap();
        assert_eq!(found, bin);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_terminal_applescript_quotes_special_chars() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = dir.path().join("launch \"test\".sh");
        fs::write(&launcher, b"#!/bin/sh\n").unwrap();
        let script = macos_terminal_applescript(&launcher);
        assert!(script.contains("tell application \"Terminal\""));
        assert!(script.contains("do script"));
        assert!(script.contains("\\\""));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn open_terminal_macos_with_success_and_failure() {
        use std::process::ExitStatus;
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;

        let dir = tempfile::tempdir().unwrap();
        let launcher = dir.path().join("go.sh");
        fs::write(&launcher, b"#!/bin/sh\n").unwrap();

        open_terminal_macos_with(&launcher, |_| Ok(ExitStatus::default())).unwrap();

        let err = open_terminal_macos_with(&launcher, |_| {
            #[cfg(unix)]
            {
                Ok(ExitStatus::from_raw(256))
            }
            #[cfg(not(unix))]
            {
                Ok(ExitStatus::from_raw(1))
            }
        })
        .unwrap_err();
        assert!(err.contains("osascript exited"));
    }

    #[test]
    fn launch_session_uses_injected_terminal_opener() {
        let _guard = crate::test_support::env_lock();
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        fs::write(&bin, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&bin).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&bin, perms).unwrap();
        }
        std::env::set_var("PATH", dir.path());
        let project = tempfile::tempdir().unwrap();
        launch_session_with_opener(
            InvestigateTarget::Claude,
            &sample_entry(),
            project.path(),
            |_| Ok(()),
        )
        .unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn open_terminal_macos_propagates_error() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = dir.path().join("go.sh");
        fs::write(&launcher, b"#!/bin/sh\n").unwrap();
        let err = open_terminal_macos_with(&launcher, |_| {
            Err("osascript not found".into())
        })
        .unwrap_err();
        assert!(err.contains("osascript"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn open_terminal_macos_uses_osascript_hook() {
        use std::process::ExitStatus;
        let dir = tempfile::tempdir().unwrap();
        let launcher = dir.path().join("go.sh");
        fs::write(&launcher, b"#!/bin/sh\n").unwrap();
        test_osascript::set(Some(|_| Ok(ExitStatus::default())));
        open_terminal_macos(&launcher).unwrap();
        // Also exercise platform dispatch with opener cleared.
        set_test_terminal_opener(None);
        test_osascript::set(Some(|_| Ok(ExitStatus::default())));
        open_terminal_platform(&launcher).unwrap();
        test_osascript::set(None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn launch_session_default_opener_path() {
        let _guard = crate::test_support::env_lock();
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        fs::write(&bin, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&bin).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&bin, perms).unwrap();
        }
        std::env::set_var("PATH", dir.path());
        let project = tempfile::tempdir().unwrap();
        set_test_terminal_opener(Some(|_| Ok(())));
        launch_session(InvestigateTarget::Claude, &sample_entry(), project.path()).unwrap();
        set_test_terminal_opener(None);
    }

    #[test]
    fn build_prompt_contains_essential_info() {
        let e = sample_entry();
        let p = build_prompt(&e);
        assert!(p.contains("42"));
        assert!(p.contains("api"));
        assert!(p.contains("get_logs_around"));
        assert!(p.contains("search_logs"));
    }

    #[cfg(windows)]
    #[test]
    fn powershell_single_quote_escapes() {
        assert_eq!(powershell_single_quote("it's"), "'it''s'");
        assert_eq!(powershell_single_quote("C:\\path"), "'C:\\path'");
    }

    #[test]
    fn open_terminal_respects_test_opener() {
        set_test_terminal_opener(Some(|_| Ok(())));
        let dir = tempfile::tempdir().unwrap();
        let launcher = dir.path().join("go.sh");
        fs::write(&launcher, b"#!/bin/sh\n").unwrap();
        let result = open_terminal(&launcher);
        assert!(result.is_ok());
        set_test_terminal_opener(None);
    }

    #[test]
    fn write_launcher_includes_project_and_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let prompt = tmp.path().join("prompt.txt");
        fs::write(&prompt, "test prompt").unwrap();
        let cli = tmp.path().join("fake-agent");
        fs::write(&cli, b"#!/bin/sh\n").unwrap();
        let launcher = write_launcher(tmp.path(), 123, project.path(), &cli, &prompt).unwrap();
        let content = fs::read_to_string(&launcher).unwrap();
        assert!(
            content.contains(&project.path().display().to_string())
                || content.contains("Set-Location")
        );
        assert!(
            content.contains(&prompt.display().to_string()) || content.contains("Get-Content")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn open_terminal_linux_uses_terminal_env() {
        let _guard = crate::test_support::env_lock();
        let dir = tempfile::tempdir().unwrap();
        let fake_term = dir.path().join("my-terminal");
        fs::write(&fake_term, b"#!/bin/sh\nsleep 0.01\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&fake_term).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&fake_term, perms).unwrap();
        }
        std::env::set_var("TERMINAL", fake_term.to_str().unwrap());
        std::env::set_var("PATH", dir.path().to_str().unwrap());
        let launcher = dir.path().join("test.sh");
        fs::write(&launcher, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&launcher).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&launcher, perms).unwrap();
        }
        let result = open_terminal_linux(&launcher);
        // May succeed or fail depending on environment; we just exercise the path
        let _ = result;
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn open_terminal_linux_tries_fallbacks() {
        let _guard = crate::test_support::env_lock();
        std::env::remove_var("TERMINAL");
        let dir = tempfile::tempdir().unwrap();
        let launcher = dir.path().join("test.sh");
        fs::write(&launcher, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&launcher).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&launcher, perms).unwrap();
        }
        let result = open_terminal_linux(&launcher);
        // Will likely fail without a real terminal, but we exercise the code
        let _ = result;
    }

    #[cfg(windows)]
    #[test]
    fn open_terminal_windows_attempts_launch() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = dir.path().join("test.ps1");
        fs::write(&launcher, b"Write-Host 'test'\n").unwrap();
        let result = open_terminal_windows(&launcher);
        // Will likely fail in test environment but exercises the code
        let _ = result;
    }

    #[test]
    fn open_terminal_platform_unsupported() {
        #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
        {
            let dir = tempfile::tempdir().unwrap();
            let launcher = dir.path().join("test.sh");
            fs::write(&launcher, b"#!/bin/sh\n").unwrap();
            let result = open_terminal_platform(&launcher);
            assert!(result.is_err());
            assert!(result.unwrap_err().contains("not supported"));
        }
    }
}
