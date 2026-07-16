//! Shell attach state, startup-file hooks, hub ensure, and init script generation.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use tracing::warn;

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 1738;

const MARKER_BEGIN: &str = "# >>> mizpah >>>";
const MARKER_END: &str = "# <<< mizpah <<<";
const HUB_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const HUB_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const HUB_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Zsh,
    Bash,
}

impl ShellKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "zsh" => Some(Self::Zsh),
            "bash" => Some(Self::Bash),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Zsh => "zsh",
            Self::Bash => "bash",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AttachState {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    pub host: String,
    pub port: u16,
}

impl Default for AttachState {
    fn default() -> Self {
        Self {
            enabled: false,
            service: None,
            host: DEFAULT_HOST.to_string(),
            port: DEFAULT_PORT,
        }
    }
}

pub fn config_dir() -> io::Result<PathBuf> {
    if let Ok(dir) = std::env::var("MIZPAH_CONFIG_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    directories::ProjectDirs::from("dev", "ethira", "mizpah")
        .map(|d| d.config_dir().to_path_buf())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "could not resolve config dir"))
}

pub fn state_path() -> io::Result<PathBuf> {
    Ok(config_dir()?.join("attach.json"))
}

pub fn hub_pid_path(port: u16) -> io::Result<PathBuf> {
    Ok(config_dir()?.join(format!("hub-{port}.pid")))
}

/// Best-effort: write this process's PID for `mzp hub stop`.
pub fn write_hub_pid(port: u16) -> io::Result<()> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    let path = hub_pid_path(port)?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut f = fs::File::create(&tmp)?;
        writeln!(f, "{}", std::process::id())?;
        f.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn read_hub_pid(port: u16) -> io::Result<Option<u32>> {
    let path = hub_pid_path(port)?;
    match fs::read_to_string(&path) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            let pid = trimmed.parse::<u32>().map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("corrupt hub PID file {}: {e}", path.display()),
                )
            })?;
            Ok(Some(pid))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn remove_hub_pid(port: u16) -> io::Result<()> {
    let path = hub_pid_path(port)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Read attach state. Missing file → disabled defaults. Corrupted JSON → error.
pub fn load_state() -> io::Result<AttachState> {
    load_state_from(&state_path()?)
}

pub fn load_state_from(path: &Path) -> io::Result<AttachState> {
    match fs::read_to_string(path) {
        Ok(raw) => {
            if raw.trim().is_empty() {
                return Ok(AttachState::default());
            }
            serde_json::from_str(&raw).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("corrupt attach state {}: {e}", path.display()),
                )
            })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(AttachState::default()),
        Err(e) => Err(e),
    }
}

/// Atomically write attach state with user-only permissions.
pub fn save_state(state: &AttachState) -> io::Result<()> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    save_state_to(&dir.join("attach.json"), state)
}

pub fn save_state_to(path: &Path, state: &AttachState) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let json = serde_json::to_vec_pretty(state)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&json)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn hub_url(host: &str, port: u16) -> String {
    format!("http://{host}:{port}")
}

pub async fn probe_hub(host: &str, port: u16) -> bool {
    let url = format!("{}/api/stats", hub_url(host, port));
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    match client.get(&url).send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// Ensure a healthy hub is reachable. Spawns a detached local hub only for loopback hosts.
/// When `project` is `None`, the current working directory is used (same as attach).
pub async fn ensure_hub(host: &str, port: u16, project: Option<&Path>) -> Result<(), String> {
    if probe_hub(host, port).await {
        return Ok(());
    }

    if !is_loopback_host(host) {
        return Err(format!(
            "hub at {} is not reachable; start it remotely or use a loopback host",
            hub_url(host, port)
        ));
    }

    let exe = crate::mcp::resolve_binary_path()
        .map_err(|e| format!("could not resolve mizpah binary: {e}"))?;

    let project_buf = project
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::current_dir().ok());
    let mut child = spawn_detached_hub(&exe, host, port, project_buf.as_deref())
        .map_err(|e| format!("failed to start hub: {e}"))?;

    let deadline = tokio::time::Instant::now() + HUB_STARTUP_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if probe_hub(host, port).await {
            // Detach: don't wait on child
            let _ = child;
            return Ok(());
        }
        // If child already exited, fail fast
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(format!("hub process exited early with {status}"));
            }
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                return Err(format!("failed monitoring hub process: {e}"));
            }
        }
        tokio::time::sleep(HUB_POLL_INTERVAL).await;
    }

    let _ = child.kill();
    let _ = child.wait();
    Err(format!(
        "hub at {} did not become healthy within {}s",
        hub_url(host, port),
        HUB_STARTUP_TIMEOUT.as_secs()
    ))
}

fn process_exists(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0) checks existence / permission without signaling.
        // SAFETY: libc::kill with signal 0 is a standard existence probe.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(windows)]
    {
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .map(|o| {
                let s = String::from_utf8_lossy(&o.stdout);
                s.contains(&pid.to_string())
            })
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

fn signal_term(pid: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        // SAFETY: sending SIGTERM to a PID we own via hub.pid.
        let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if rc == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        Err(format!("failed to SIGTERM pid {pid}: {err}"))
    }
    #[cfg(windows)]
    {
        // Soft stop is not reliable on Windows; terminate immediately.
        signal_kill(pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(format!("cannot signal pid {pid} on this platform"))
    }
}

fn signal_kill(pid: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        // SAFETY: sending SIGKILL after stop timeout.
        let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        if rc == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        Err(format!("failed to SIGKILL pid {pid}: {err}"))
    }
    #[cfg(windows)]
    {
        let status = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F", "/T"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("failed to taskkill pid {pid}: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            // Exit code 128 = process not found
            if status.code() == Some(128) {
                Ok(())
            } else {
                Err(format!("taskkill pid {pid} failed with {status}"))
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(format!("cannot kill pid {pid} on this platform"))
    }
}

/// Run `mzp hub start`.
pub async fn run_hub_start(
    host: String,
    port: u16,
    project: Option<PathBuf>,
) -> Result<(), String> {
    let url = hub_url(&host, port);
    if probe_hub(&host, port).await {
        eprintln!("mizpah hub already running at {url}");
        return Ok(());
    }
    ensure_hub(&host, port, project.as_deref()).await?;
    eprintln!("mizpah hub started at {url}");
    Ok(())
}

/// Run `mzp hub stop`.
pub async fn run_hub_stop(host: String, port: u16) -> Result<(), String> {
    let url = hub_url(&host, port);
    let pid = match read_hub_pid(port) {
        Ok(Some(p)) => p,
        Ok(None) => {
            if probe_hub(&host, port).await {
                return Err(format!(
                    "hub at {url} appears running but PID file is missing\n\
                     hint: stop the process listening on port {port} manually, then retry"
                ));
            }
            eprintln!("mizpah hub already stopped");
            return Ok(());
        }
        Err(e) => return Err(format!("failed to read hub PID file: {e}")),
    };

    if !process_exists(pid) {
        let _ = remove_hub_pid(port);
        if probe_hub(&host, port).await {
            return Err(format!(
                "hub at {url} appears running but PID file is stale (pid {pid})\n\
                 hint: stop the process listening on port {port} manually, then retry"
            ));
        }
        eprintln!("mizpah hub already stopped (stale PID file removed)");
        return Ok(());
    }

    signal_term(pid)?;

    let deadline = tokio::time::Instant::now() + HUB_STOP_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if !process_exists(pid) && !probe_hub(&host, port).await {
            break;
        }
        if !process_exists(pid) {
            break;
        }
        tokio::time::sleep(HUB_POLL_INTERVAL).await;
    }

    if process_exists(pid) {
        signal_kill(pid)?;
        let kill_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < kill_deadline {
            if !process_exists(pid) {
                break;
            }
            tokio::time::sleep(HUB_POLL_INTERVAL).await;
        }
    }

    let _ = remove_hub_pid(port);

    if process_exists(pid) {
        return Err(format!("hub process pid {pid} did not exit"));
    }
    if probe_hub(&host, port).await {
        return Err(format!(
            "hub at {url} is still reachable after stopping pid {pid}"
        ));
    }

    eprintln!("mizpah hub stopped");
    Ok(())
}

/// Run `mzp hub restart`.
pub async fn run_hub_restart(
    host: String,
    port: u16,
    project: Option<PathBuf>,
) -> Result<(), String> {
    run_hub_stop(host.clone(), port).await?;
    run_hub_start(host, port, project).await
}

fn spawn_detached_hub(
    exe: &Path,
    host: &str,
    port: u16,
    project: Option<&Path>,
) -> io::Result<std::process::Child> {
    let mut cmd = Command::new(exe);
    cmd.args(["--host", host, "--port", &port.to_string(), "--no-open"]);
    if let Some(project) = project {
        cmd.arg("--project").arg(project);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // New session so the hub outlives the attach CLI.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    cmd.spawn()
}

fn home_dir() -> io::Result<PathBuf> {
    directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME not found"))
}

fn zshrc_path() -> io::Result<PathBuf> {
    let home = home_dir()?;
    let zdotdir = std::env::var_os("ZDOTDIR").map(PathBuf::from);
    Ok(zdotdir.unwrap_or(home).join(".zshrc"))
}

fn bashrc_path() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".bashrc"))
}

/// First existing bash login file, or `.bash_profile` to create.
fn bash_login_path() -> io::Result<PathBuf> {
    let home = home_dir()?;
    for name in [".bash_profile", ".bash_login", ".profile"] {
        let p = home.join(name);
        if p.is_file() {
            return Ok(p);
        }
    }
    Ok(home.join(".bash_profile"))
}

/// Escape a path/string for safe embedding in single-quoted shell contexts.
pub fn shell_single_quote(s: &str) -> String {
    // 'foo'bar' → 'foo'\''bar'
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Pure transform: install or replace the managed marker block.
pub fn apply_marker_block(existing: &str, block_body: &str) -> Result<String, String> {
    let begin = existing.find(MARKER_BEGIN);
    let end = existing.find(MARKER_END);

    match (begin, end) {
        (None, None) => {
            let mut out = existing.to_string();
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(MARKER_BEGIN);
            out.push('\n');
            out.push_str(block_body);
            if !block_body.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(MARKER_END);
            out.push('\n');
            Ok(out)
        }
        (Some(b), Some(e)) if e > b => {
            let after_end = e + MARKER_END.len();
            let mut out = String::new();
            out.push_str(&existing[..b]);
            out.push_str(MARKER_BEGIN);
            out.push('\n');
            out.push_str(block_body);
            if !block_body.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(MARKER_END);
            let rest = &existing[after_end..];
            if !rest.is_empty() && !rest.starts_with('\n') {
                out.push('\n');
            }
            out.push_str(rest);
            if !out.ends_with('\n') {
                out.push('\n');
            }
            Ok(out)
        }
        _ => {
            Err("malformed mizpah marker block in shell startup file (unmatched begin/end)".into())
        }
    }
}

fn hook_line(bin: &Path, shell: ShellKind) -> String {
    let quoted = shell_single_quote(&bin.display().to_string());
    format!("eval \"$({quoted} __shell-init {})\"", shell.as_str())
}

fn write_startup_file(path: &Path, content: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Preserve permissions when overwriting.
    let mode = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::metadata(path).ok().map(|m| m.permissions().mode())
        }
        #[cfg(not(unix))]
        {
            None::<u32>
        }
    };

    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        if !content.ends_with('\n') {
            f.write_all(b"\n")?;
        }
        f.sync_all()?;
    }
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(mode));
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

fn install_hook_file(path: &Path, bin: &Path, shell: ShellKind) -> Result<(), String> {
    let existing = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let body = hook_line(bin, shell);
    let updated = apply_marker_block(&existing, &body)?;
    if updated == existing {
        return Ok(());
    }
    write_startup_file(path, &updated).map_err(|e| format!("{}: {e}", path.display()))
}

/// Install managed hooks into zsh/bash startup files.
pub fn install_shell_hooks(bin: &Path) -> Result<Vec<PathBuf>, String> {
    let mut touched = Vec::new();

    let zshrc = zshrc_path().map_err(|e| e.to_string())?;
    install_hook_file(&zshrc, bin, ShellKind::Zsh)?;
    touched.push(zshrc);

    let bashrc = bashrc_path().map_err(|e| e.to_string())?;
    install_hook_file(&bashrc, bin, ShellKind::Bash)?;
    touched.push(bashrc);

    let login = bash_login_path().map_err(|e| e.to_string())?;
    // Ensure login shells source bashrc when we create/update bash_profile.
    // Only inject our marker; if creating a new .bash_profile, also source .bashrc.
    let login_existing = match fs::read_to_string(&login) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // New file: source bashrc then our hook.
            String::new()
        }
        Err(e) => return Err(format!("{}: {e}", login.display())),
    };

    let mut body = String::new();
    if login_existing.is_empty() {
        body.push_str("# Created by mizpah attach — source interactive bashrc for login shells\n");
        body.push_str("if [ -f \"$HOME/.bashrc\" ]; then\n  . \"$HOME/.bashrc\"\nfi\n");
    }
    body.push_str(&hook_line(bin, ShellKind::Bash));

    // For login file: if empty we wrote source+hook as body; if non-empty only the hook line.
    let block_body = if login_existing.is_empty() {
        body
    } else {
        hook_line(bin, ShellKind::Bash)
    };
    let updated = apply_marker_block(&login_existing, &block_body)?;
    if updated != login_existing {
        write_startup_file(&login, &updated).map_err(|e| format!("{}: {e}", login.display()))?;
    }
    touched.push(login);

    Ok(touched)
}

/// Generate shell init snippet printed by `__shell-init`. Empty when disabled.
pub fn generate_init_script(shell: ShellKind, state: &AttachState, bin: &Path) -> String {
    if !state.enabled {
        return String::new();
    }

    let bin_q = shell_single_quote(&bin.display().to_string());
    let preexec_hooks = match shell {
        ShellKind::Zsh => ZSH_PREEXEC_HOOKS,
        ShellKind::Bash => BASH_PREEXEC_HOOKS,
    };
    // Wrap in a function so `return` is valid under bash `eval`.
    // FD 9 goes only to the forwarder (control frames never hit the TTY).
    // stdout/stderr still go through tee so the user sees them.
    format!(
        r#"__mizpah_shell_attach() {{
  case "$-" in
    *i*) ;;
    *) return 0 ;;
  esac
  [ -n "${{MIZPAH_SHELL_ATTACHED:-}}" ] && return 0
  [ -t 1 ] || return 0
  export MIZPAH_SHELL_ATTACHED=1
  __mizpah_cwd_service="$(pwd -P 2>/dev/null || echo "${{PWD:-unknown}}")"
  exec 9> >({bin} __shell-forward --tty-service "$__mizpah_cwd_service" 2>/dev/null)
  exec > >(tee /dev/tty >&9) 2>&1
  __mizpah_emit_meta() {{
    local __mizpah_cwd __mizpah_cmd_b64
    __mizpah_cwd="$(pwd -P 2>/dev/null || echo "${{PWD:-unknown}}")"
    if command -v base64 >/dev/null 2>&1; then
      __mizpah_cmd_b64="$(printf '%s' "$1" | base64 2>/dev/null | tr -d '\n\r')"
    else
      __mizpah_cmd_b64=
    fi
    printf '\036MZP\036%s\036%s\n' "$__mizpah_cwd" "$__mizpah_cmd_b64" >&9
  }}
{preexec_hooks}}}
__mizpah_shell_attach
unset -f __mizpah_shell_attach 2>/dev/null || true
"#,
        bin = bin_q,
        preexec_hooks = preexec_hooks,
    )
}

/// zsh: use add-zsh-hook when available so we don't clobber an existing preexec.
const ZSH_PREEXEC_HOOKS: &str = r#"  __mizpah_preexec() {
    __mizpah_emit_meta "$1"
  }
  if autoload -Uz add-zsh-hook 2>/dev/null; then
    add-zsh-hook preexec __mizpah_preexec
  else
    preexec() { __mizpah_preexec "$@"; }
  fi
"#;

/// bash: DEBUG trap armed by PROMPT_COMMAND so we only capture user commands.
const BASH_PREEXEC_HOOKS: &str = r#"  __mizpah_preexec_active=
  __mizpah_debug_trap() {
    if [ -n "${__mizpah_preexec_active:-}" ]; then
      __mizpah_preexec_active=
      __mizpah_emit_meta "$BASH_COMMAND"
    fi
  }
  trap '__mizpah_debug_trap' DEBUG
  if [ -n "${PROMPT_COMMAND:-}" ]; then
    PROMPT_COMMAND="__mizpah_preexec_active=; ${PROMPT_COMMAND}; __mizpah_preexec_active=1"
  else
    PROMPT_COMMAND='__mizpah_preexec_active=1'
  fi
"#;

/// Run `mzp attach`.
pub async fn run_attach(service: Option<String>, host: String, port: u16) -> Result<(), String> {
    let bin =
        crate::mcp::resolve_binary_path().map_err(|e| format!("could not resolve binary: {e}"))?;

    let service = service
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    // Install hooks before enabling so a half-done attach doesn't leave enabled=true.
    let touched = install_shell_hooks(&bin)?;
    ensure_hub(&host, port, None).await?;

    let state = AttachState {
        enabled: true,
        service,
        host: host.clone(),
        port,
    };
    save_state(&state).map_err(|e| format!("failed to save attach state: {e}"))?;

    let url = hub_url(&host, port);
    eprintln!("mizpah attach enabled");
    eprintln!("  hub: {url}");
    eprintln!("  open UI: mzp open");
    eprintln!("  new interactive shells will forward stdout/stderr");
    for p in &touched {
        eprintln!("  hook: {}", p.display());
    }
    Ok(())
}

/// Run `mzp detach`.
pub fn run_detach() -> Result<(), String> {
    let mut state = load_state().map_err(|e| e.to_string())?;
    if !state.enabled {
        eprintln!("mizpah attach already disabled");
        return Ok(());
    }
    state.enabled = false;
    save_state(&state).map_err(|e| format!("failed to save attach state: {e}"))?;
    eprintln!("mizpah attach disabled (hub left running; hooks remain for re-attach)");
    Ok(())
}

/// Resolve host/port for `mzp open`: flags if provided specially, else state, else defaults.
pub fn resolve_open_target(
    host_flag: Option<String>,
    port_flag: Option<u16>,
) -> Result<(String, u16), String> {
    let state = load_state().unwrap_or_default();
    let host = host_flag.filter(|h| !h.is_empty()).unwrap_or_else(|| {
        if state.host.is_empty() {
            DEFAULT_HOST.to_string()
        } else {
            state.host.clone()
        }
    });
    let port = port_flag.unwrap_or(if state.port == 0 {
        DEFAULT_PORT
    } else {
        state.port
    });
    Ok((host, port))
}

pub async fn run_open(host: String, port: u16) -> Result<(), String> {
    if !probe_hub(&host, port).await {
        return Err(format!(
            "hub at {} is not reachable\n\
             hint: run `mzp attach` or pipe logs into `mzp` first",
            hub_url(&host, port)
        ));
    }
    let url = hub_url(&host, port);
    open::that(&url).map_err(|e| format!("failed to open browser: {e}"))?;
    eprintln!("opened {url}");
    Ok(())
}

/// Print init script to stdout (for eval from rc).
pub fn run_shell_init(shell_name: &str) -> Result<(), String> {
    let shell = ShellKind::parse(shell_name)
        .ok_or_else(|| format!("unknown shell `{shell_name}` (expected zsh or bash)"))?;
    let state = load_state().unwrap_or_else(|e| {
        warn!(error = %e, "failed to load attach state; treating as disabled");
        AttachState::default()
    });
    let bin =
        crate::mcp::resolve_binary_path().map_err(|e| format!("could not resolve binary: {e}"))?;
    let script = generate_init_script(shell, &state, &bin);
    print!("{script}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_single_quote_escapes_apostrophe() {
        assert_eq!(shell_single_quote("a'b"), "'a'\\''b'");
        assert_eq!(shell_single_quote("/usr/bin/mzp"), "'/usr/bin/mzp'");
    }

    #[test]
    fn apply_marker_first_install() {
        let out = apply_marker_block("", "eval hello").unwrap();
        assert!(out.contains(MARKER_BEGIN));
        assert!(out.contains("eval hello"));
        assert!(out.contains(MARKER_END));
    }

    #[test]
    fn apply_marker_idempotent_replace() {
        let once = apply_marker_block("# existing\n", "eval old").unwrap();
        let twice = apply_marker_block(&once, "eval new").unwrap();
        assert_eq!(twice.matches(MARKER_BEGIN).count(), 1);
        assert!(twice.contains("eval new"));
        assert!(!twice.contains("eval old"));
        assert!(twice.contains("# existing"));
    }

    #[test]
    fn apply_marker_malformed_errors() {
        let bad = format!("{MARKER_BEGIN}\nno end\n");
        assert!(apply_marker_block(&bad, "x").is_err());
    }

    #[test]
    fn generate_init_empty_when_disabled() {
        let state = AttachState::default();
        let script = generate_init_script(ShellKind::Zsh, &state, Path::new("/bin/mzp"));
        assert!(script.is_empty());
    }

    #[test]
    fn generate_init_contains_guards_when_enabled() {
        let state = AttachState {
            enabled: true,
            service: None,
            host: DEFAULT_HOST.into(),
            port: DEFAULT_PORT,
        };
        let script = generate_init_script(ShellKind::Bash, &state, Path::new("/opt/mzp"));
        assert!(script.contains("MIZPAH_SHELL_ATTACHED"));
        assert!(script.contains("__shell-forward"));
        assert!(script.contains("tee /dev/tty"));
        assert!(script.contains("pwd -P"));
        assert!(script.contains("'/opt/mzp'"));
        assert!(script.contains("exec 9>"));
        assert!(script.contains("__mizpah_emit_meta"));
        assert!(script.contains("\\036MZP\\036"));
        assert!(script.contains("BASH_COMMAND"));
        assert!(script.contains("PROMPT_COMMAND"));
    }

    #[test]
    fn generate_init_zsh_uses_preexec_hook() {
        let state = AttachState {
            enabled: true,
            service: None,
            host: DEFAULT_HOST.into(),
            port: DEFAULT_PORT,
        };
        let script = generate_init_script(ShellKind::Zsh, &state, Path::new("/opt/mzp"));
        assert!(script.contains("add-zsh-hook preexec"));
        assert!(script.contains("__mizpah_preexec"));
        assert!(!script.contains("BASH_COMMAND"));
    }

    #[test]
    fn shell_kind_parse() {
        assert_eq!(ShellKind::parse("zsh"), Some(ShellKind::Zsh));
        assert_eq!(ShellKind::parse("bash"), Some(ShellKind::Bash));
        assert_eq!(ShellKind::parse("fish"), None);
    }

    #[test]
    fn is_loopback() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("192.168.1.1"));
        assert!(!is_loopback_host("example.com"));
    }

    #[test]
    fn state_roundtrip_in_temp_config() {
        // Exercise serialize shape
        let s = AttachState {
            enabled: true,
            service: Some("api".into()),
            host: "127.0.0.1".into(),
            port: 1738,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: AttachState = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn save_load_state_via_path() {
        let dir = std::env::temp_dir().join(format!(
            "mizpah-attach-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("attach.json");

        let missing = load_state_from(&path).unwrap();
        assert!(!missing.enabled);

        let s = AttachState {
            enabled: true,
            service: Some("dev".into()),
            host: "127.0.0.1".into(),
            port: 1738,
        };
        save_state_to(&path, &s).unwrap();
        let loaded = load_state_from(&path).unwrap();
        assert_eq!(loaded, s);

        let mut disabled = loaded;
        disabled.enabled = false;
        save_state_to(&path, &disabled).unwrap();
        assert!(!load_state_from(&path).unwrap().enabled);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_open_target_flags_override_defaults() {
        // Without relying on global config: flags win when provided.
        let (h, p) = resolve_open_target(Some("127.0.0.1".into()), Some(1738)).unwrap();
        assert_eq!(h, "127.0.0.1");
        assert_eq!(p, 1738);

        let (h, p) = resolve_open_target(Some("10.0.0.1".into()), Some(9999)).unwrap();
        assert_eq!(h, "10.0.0.1");
        assert_eq!(p, 9999);
    }

    #[test]
    fn corrupt_state_errors() {
        let dir = std::env::temp_dir().join(format!(
            "mizpah-corrupt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("attach.json");
        fs::write(&path, "{not-json").unwrap();
        assert!(load_state_from(&path).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hub_pid_roundtrip_and_stale_cleanup() {
        let dir = std::env::temp_dir().join(format!(
            "mizpah-hub-pid-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // SAFETY: test-only env override for config dir isolation.
        std::env::set_var("MIZPAH_CONFIG_DIR", &dir);

        let port = 1738u16;
        assert!(read_hub_pid(port).unwrap().is_none());

        write_hub_pid(port).unwrap();
        assert_eq!(read_hub_pid(port).unwrap(), Some(std::process::id()));
        assert!(hub_pid_path(port).unwrap().starts_with(&dir));

        remove_hub_pid(port).unwrap();
        assert!(read_hub_pid(port).unwrap().is_none());

        // Stale / corrupt PID file
        fs::write(hub_pid_path(port).unwrap(), "not-a-pid\n").unwrap();
        assert!(read_hub_pid(port).is_err());
        remove_hub_pid(port).unwrap();

        // Nonexistent PID (avoid u32::MAX — casts to -1 / kill(-1) on unix)
        assert!(!process_exists(999_999_999));

        std::env::remove_var("MIZPAH_CONFIG_DIR");
        let _ = fs::remove_dir_all(&dir);
    }
}
