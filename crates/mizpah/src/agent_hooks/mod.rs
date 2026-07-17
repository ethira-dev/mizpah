//! Cursor / Claude Code lifecycle hooks → Mizpah hub ingest.
//!
//! `mzp attach cursor|claude` merges observe-only command hooks into user-global
//! configs. Each hook invokes `mzp __hook-forward`, which POSTs a structured
//! envelope to `/api/ingest` and always exits 0 with empty stdout.

mod claude;
mod cursor;
mod forward;
mod shared;
mod state;

pub use claude::{run_attach_claude, run_detach_claude};
pub use cursor::{run_attach_cursor, run_detach_cursor};
pub use forward::run_hook_forward;
pub use state::HookSource;

use crate::hub;
use crate::mcp;
use crate::shell_attach;
use shared::{
    claude_settings_path, cursor_hooks_path, managed_command, read_file_or_empty, write_config_file,
};
use state::{load_state, save_state, SourceState};

pub fn run_detach_all() -> Result<(), String> {
    shell_attach::run_detach()?;
    run_detach_cursor()?;
    run_detach_claude()?;
    Ok(())
}

pub(super) async fn run_attach_source(
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
            let (out, changed) = cursor::merge_cursor_hooks(&existing, &command)?;
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
            let (out, changed) = claude::merge_claude_hooks(&existing, &command)?;
            if changed || existing.trim().is_empty() {
                write_config_file(&path, &out).map_err(|e| e.to_string())?;
            }
            (path, changed || existing.trim().is_empty())
        }
    };

    hub::ensure_hub(&host, port, None, false)
        .await
        .map_err(|e| e.to_string())?;

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

    let url = hub::hub_url(&host, port);
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

pub(super) fn run_detach_source(source: HookSource) -> Result<(), String> {
    let path = match source {
        HookSource::Cursor => cursor_hooks_path()
            .ok_or_else(|| "could not resolve home directory for Cursor hooks".to_string())?,
        HookSource::Claude => claude_settings_path()
            .ok_or_else(|| "could not resolve home directory for Claude settings".to_string())?,
    };

    let existing = read_file_or_empty(&path).map_err(|e| e.to_string())?;
    if !existing.trim().is_empty() {
        let (out, changed) = match source {
            HookSource::Cursor => cursor::remove_cursor_hooks(&existing)?,
            HookSource::Claude => claude::remove_claude_hooks(&existing)?,
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

#[cfg(test)]
mod tests {
    use super::shared::{is_managed_command, managed_command};
    use super::HookSource;
    use std::path::Path;

    #[test]
    fn managed_command_contains_marker() {
        let cmd = managed_command(Path::new("/usr/local/bin/mzp"), HookSource::Cursor);
        assert!(is_managed_command(&cmd, HookSource::Cursor));
        assert!(!is_managed_command(&cmd, HookSource::Claude));
        assert!(cmd.contains("__hook-forward --source cursor"));
    }
}
