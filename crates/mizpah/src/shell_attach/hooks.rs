//! Shell startup-file hooks and init script generation.

use super::state::{AttachState, ShellKind};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const MARKER_BEGIN: &str = "# >>> mizpah >>>";
const MARKER_END: &str = "# <<< mizpah <<<";

fn home_dir() -> io::Result<PathBuf> {
    crate::util::home_dir().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME not found"))
}

pub(crate) fn zshrc_path() -> io::Result<PathBuf> {
    let home = home_dir()?;
    let zdotdir = std::env::var_os("ZDOTDIR").map(PathBuf::from);
    Ok(zdotdir.unwrap_or(home).join(".zshrc"))
}

pub(crate) fn bashrc_path() -> io::Result<PathBuf> {
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

pub use crate::util::shell_single_quote;

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

pub(crate) fn hook_line(bin: &Path, shell: ShellKind) -> String {
    let quoted = shell_single_quote(&bin.display().to_string());
    format!("eval \"$({quoted} __shell-init {})\"", shell.as_str())
}

pub(crate) fn write_startup_file(path: &Path, content: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
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

pub(crate) fn install_hook_file(path: &Path, bin: &Path, shell: ShellKind) -> Result<(), String> {
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

/// Install hooks into the given startup files (for tests and [`install_shell_hooks`]).
pub fn install_shell_hooks_at(
    zshrc: &Path,
    bashrc: &Path,
    login: &Path,
    bin: &Path,
) -> Result<Vec<PathBuf>, String> {
    let mut touched = Vec::new();

    install_hook_file(zshrc, bin, ShellKind::Zsh)?;
    touched.push(zshrc.to_path_buf());

    install_hook_file(bashrc, bin, ShellKind::Bash)?;
    touched.push(bashrc.to_path_buf());

    let login_existing = match fs::read_to_string(login) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", login.display())),
    };

    let mut body = String::new();
    if login_existing.is_empty() {
        body.push_str("# Created by mizpah attach — source interactive bashrc for login shells\n");
        body.push_str("if [ -f \"$HOME/.bashrc\" ]; then\n  . \"$HOME/.bashrc\"\nfi\n");
    }
    body.push_str(&hook_line(bin, ShellKind::Bash));

    let block_body = if login_existing.is_empty() {
        body
    } else {
        hook_line(bin, ShellKind::Bash)
    };
    let updated = apply_marker_block(&login_existing, &block_body)?;
    if updated != login_existing {
        write_startup_file(login, &updated).map_err(|e| format!("{}: {e}", login.display()))?;
    }
    touched.push(login.to_path_buf());

    Ok(touched)
}

/// Install managed hooks into zsh/bash startup files.
pub fn install_shell_hooks(bin: &Path) -> Result<Vec<PathBuf>, String> {
    let zshrc = zshrc_path().map_err(|e| e.to_string())?;
    let bashrc = bashrc_path().map_err(|e| e.to_string())?;
    let login = bash_login_path().map_err(|e| e.to_string())?;
    install_shell_hooks_at(&zshrc, &bashrc, &login, bin)
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
  exec 9> >({bin_q} __shell-forward --tty-service "$__mizpah_cwd_service" 2>/dev/null)
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
    )
}

const ZSH_PREEXEC_HOOKS: &str = r#"  __mizpah_preexec() {
    __mizpah_emit_meta "$1"
  }
  if autoload -Uz add-zsh-hook 2>/dev/null; then
    add-zsh-hook preexec __mizpah_preexec
  else
    preexec() { __mizpah_preexec "$@"; }
  fi
"#;

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

/// Build init script for `__shell-init` (stdout printing is handled by [`run_shell_init`]).
pub fn prepare_shell_init(shell_name: &str) -> Result<String, String> {
    use super::state::{load_state, AttachState};
    use tracing::warn;

    let shell = ShellKind::parse(shell_name)
        .ok_or_else(|| format!("unknown shell `{shell_name}` (expected zsh or bash)"))?;
    let state = load_state().unwrap_or_else(|e| {
        warn!(error = %e, "failed to load attach state; treating as disabled");
        AttachState::default()
    });
    let bin =
        crate::mcp::resolve_binary_path().map_err(|e| format!("could not resolve binary: {e}"))?;
    Ok(generate_init_script(shell, &state, &bin))
}

/// Print init script to stdout (for eval from rc).
pub fn run_shell_init(shell_name: &str) -> Result<(), String> {
    let script = prepare_shell_init(shell_name)?;
    print!("{script}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell_attach::{save_state, AttachState, DEFAULT_HOST, DEFAULT_PORT};

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
    fn apply_marker_replace_preserves_trailing_content() {
        let existing = format!("before\n{MARKER_BEGIN}\nold\n{MARKER_END}after");
        let out = apply_marker_block(&existing, "new hook").unwrap();
        assert!(out.contains("before"));
        assert!(out.contains("new hook"));
        assert!(out.contains("after"));
    }

    #[test]
    fn apply_marker_end_only_errors() {
        let bad = format!("x\n{MARKER_END}\n");
        assert!(apply_marker_block(&bad, "body").is_err());
    }

    #[test]
    fn hook_line_quotes_binary_path() {
        let line = hook_line(Path::new("/opt/mizpah"), ShellKind::Zsh);
        assert!(line.contains("'__shell-init zsh'") || line.contains("__shell-init zsh"));
        assert!(line.contains("/opt/mizpah"));
    }

    #[test]
    fn write_startup_file_creates_and_appends_newline() {
        let dir = std::env::temp_dir().join(format!("mizpah-hook-write-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nested/.zshrc");
        write_startup_file(&path, "export FOO=1").unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.ends_with('\n'));
        assert!(content.contains("export FOO=1"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_hook_file_idempotent() {
        let dir = std::env::temp_dir().join(format!("mizpah-hook-install-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let rc = dir.join(".zshrc");
        let bin = dir.join("mzp");
        fs::write(&bin, b"").unwrap();
        install_hook_file(&rc, &bin, ShellKind::Zsh).unwrap();
        let once = fs::read_to_string(&rc).unwrap();
        install_hook_file(&rc, &bin, ShellKind::Zsh).unwrap();
        let twice = fs::read_to_string(&rc).unwrap();
        assert_eq!(once, twice);
        assert!(twice.contains(MARKER_BEGIN));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_shell_hooks_at_creates_login_bootstrap() {
        use crate::test_support::env_lock;
        let _guard = env_lock();
        let home = std::env::temp_dir().join(format!("mizpah-hooks-home-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);

        let bin = home.join("bin/mzp");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fs::write(&bin, b"").unwrap();

        let zshrc = home.join(".zshrc");
        let bashrc = home.join(".bashrc");
        let login = home.join(".bash_profile");
        let touched = install_shell_hooks_at(&zshrc, &bashrc, &login, &bin).unwrap();
        assert_eq!(touched.len(), 3);
        let login_content = fs::read_to_string(&login).unwrap();
        assert!(login_content.contains(".bashrc"));
        assert!(login_content.contains(MARKER_BEGIN));

        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn bash_login_path_prefers_existing_profile() {
        use crate::test_support::env_lock;
        let _guard = env_lock();
        let home = std::env::temp_dir().join(format!("mizpah-bash-login-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join(".profile"), "# profile\n").unwrap();
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);
        let path = bash_login_path().unwrap();
        assert_eq!(path, home.join(".profile"));
        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn zshrc_path_honors_zdotdir() {
        use crate::test_support::env_lock;
        let _guard = env_lock();
        let home = std::env::temp_dir().join(format!("mizpah-zdot-{}", std::process::id()));
        let zdot = home.join("zdot");
        fs::create_dir_all(&zdot).unwrap();
        let old_home = std::env::var_os("HOME");
        let old_zdot = std::env::var_os("ZDOTDIR");
        std::env::set_var("HOME", &home);
        std::env::set_var("ZDOTDIR", &zdot);
        assert_eq!(zshrc_path().unwrap(), zdot.join(".zshrc"));
        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match old_zdot {
            Some(v) => std::env::set_var("ZDOTDIR", v),
            None => std::env::remove_var("ZDOTDIR"),
        }
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn prepare_shell_init_unknown_shell_errors() {
        assert!(prepare_shell_init("fish")
            .unwrap_err()
            .contains("unknown shell"));
    }

    #[test]
    fn prepare_shell_init_disabled_is_empty() {
        use crate::test_support::env_lock;
        let _guard = env_lock();
        let dir = std::env::temp_dir().join(format!("mizpah-init-off-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let old = std::env::var_os("MIZPAH_CONFIG_DIR");
        std::env::set_var("MIZPAH_CONFIG_DIR", &dir);
        save_state(&AttachState::default()).unwrap();
        let script = prepare_shell_init("zsh").unwrap();
        assert!(script.is_empty());
        match old {
            Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
            None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_marker_first_install_without_trailing_newline() {
        let out = apply_marker_block("export X=1", "eval hook").unwrap();
        assert!(out.starts_with("export X=1\n"));
        assert!(out.contains("eval hook"));
    }

    #[test]
    fn apply_marker_begin_only_errors() {
        let bad = format!("{MARKER_BEGIN}\nbody\n");
        assert!(apply_marker_block(&bad, "x").is_err());
    }

    #[test]
    fn prepare_shell_init_enabled_includes_forward() {
        use crate::test_support::env_lock;
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("MIZPAH_CONFIG_DIR", dir.path());
        save_state(&AttachState {
            enabled: true,
            service: Some("api".into()),
            host: DEFAULT_HOST.into(),
            port: DEFAULT_PORT,
        })
        .unwrap();
        let script = prepare_shell_init("bash").unwrap();
        assert!(script.contains("__shell-forward"));
        std::env::remove_var("MIZPAH_CONFIG_DIR");
    }

    #[test]
    fn run_shell_init_prints_to_stdout() {
        use crate::test_support::env_lock;
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("MIZPAH_CONFIG_DIR", dir.path());
        save_state(&AttachState::default()).unwrap();
        run_shell_init("zsh").unwrap();
        std::env::remove_var("MIZPAH_CONFIG_DIR");
    }

    #[test]
    fn bash_login_path_defaults_to_bash_profile() {
        use crate::test_support::env_lock;
        let _guard = env_lock();
        let home = std::env::temp_dir().join(format!("mizpah-bash-def-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);
        assert_eq!(bash_login_path().unwrap(), home.join(".bash_profile"));
        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn install_shell_hooks_at_updates_existing_login() {
        let dir = std::env::temp_dir().join(format!("mizpah-login-upd-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let zshrc = dir.join(".zshrc");
        let bashrc = dir.join(".bashrc");
        let login = dir.join(".bash_profile");
        fs::write(&login, "# existing login\n").unwrap();
        let bin = dir.join("mzp");
        fs::write(&bin, b"").unwrap();
        install_shell_hooks_at(&zshrc, &bashrc, &login, &bin).unwrap();
        let content = fs::read_to_string(&login).unwrap();
        assert!(content.contains("# existing login"));
        assert!(content.contains(MARKER_BEGIN));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bash_login_path_prefers_bash_login_file() {
        use crate::test_support::env_lock;
        let _guard = env_lock();
        let home = std::env::temp_dir().join(format!("mizpah-bash-login-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join(".bash_login"), "# login\n").unwrap();
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);
        assert_eq!(bash_login_path().unwrap(), home.join(".bash_login"));
        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn shell_rc_paths_under_home() {
        use crate::test_support::env_lock;
        let _guard = env_lock();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        assert_eq!(zshrc_path().unwrap(), home.path().join(".zshrc"));
        assert_eq!(bashrc_path().unwrap(), home.path().join(".bashrc"));
        std::env::remove_var("HOME");
    }
}
