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
    run_attach_source_at(source, service, host, port, None, None).await
}

pub(crate) async fn run_attach_source_at(
    source: HookSource,
    service: Option<String>,
    host: String,
    port: u16,
    cursor_path: Option<&std::path::Path>,
    claude_path: Option<&std::path::Path>,
) -> Result<(), String> {
    let bin = mcp::resolve_binary_path().map_err(|e| format!("could not resolve binary: {e}"))?;
    let command = managed_command(&bin, source);
    let service = service
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| source.default_service().to_string());

    let (path, merged) = match source {
        HookSource::Cursor => {
            let path = cursor_path
                .map(std::path::Path::to_path_buf)
                .or_else(cursor_hooks_path)
                .ok_or_else(|| "could not resolve home directory for Cursor hooks".to_string())?;
            let existing = read_file_or_empty(&path).map_err(|e| e.to_string())?;
            let (out, changed) = cursor::merge_cursor_hooks(&existing, &command)?;
            if changed || existing.trim().is_empty() {
                write_config_file(&path, &out).map_err(|e| e.to_string())?;
            }
            (path, changed || existing.trim().is_empty())
        }
        HookSource::Claude => {
            let path = claude_path
                .map(std::path::Path::to_path_buf)
                .or_else(claude_settings_path)
                .ok_or_else(|| {
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
    run_detach_source_at(source, None, None)
}

pub(crate) fn run_detach_source_at(
    source: HookSource,
    cursor_path: Option<&std::path::Path>,
    claude_path: Option<&std::path::Path>,
) -> Result<(), String> {
    let path = match source {
        HookSource::Cursor => cursor_path
            .map(std::path::Path::to_path_buf)
            .or_else(cursor_hooks_path)
            .ok_or_else(|| "could not resolve home directory for Cursor hooks".to_string())?,
        HookSource::Claude => claude_path
            .map(std::path::Path::to_path_buf)
            .or_else(claude_settings_path)
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
    use super::shared::{is_managed_command, managed_command, read_file_or_empty};
    use super::state::{load_state, save_state, AgentHooksState, SourceState};
    use super::{run_attach_source_at, run_detach_all, run_detach_source_at, HookSource};
    use crate::test_support::env_lock;
    use std::path::Path;

    fn with_agent_home<F: FnOnce(&std::path::Path, &std::path::Path)>(f: F) {
        let _guard = env_lock();
        let home = std::env::temp_dir().join(format!(
            "mizpah-agent-home-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config = home.join("cfg");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(home.join(".cursor")).unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let old_home = std::env::var_os("HOME");
        let old_config = std::env::var_os("MIZPAH_CONFIG_DIR");
        std::env::set_var("HOME", &home);
        std::env::set_var("MIZPAH_CONFIG_DIR", &config);
        f(&home, &config);
        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match old_config {
            Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
            None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn managed_command_contains_marker() {
        let cmd = managed_command(Path::new("/usr/local/bin/mzp"), HookSource::Cursor);
        assert!(is_managed_command(&cmd, HookSource::Cursor));
        assert!(!is_managed_command(&cmd, HookSource::Claude));
        assert!(cmd.contains("__hook-forward --source cursor"));
    }

    #[tokio::test]
    async fn run_attach_source_cursor_writes_hooks_and_state() {
        let (hub_url, _store) = crate::test_support::spawn_test_hub().await;
        let url = url::Url::parse(&hub_url).unwrap();
        let host = url.host_str().unwrap_or("127.0.0.1").to_string();
        let port = url.port().unwrap_or(80);

        let _guard = env_lock();
        let home = std::env::temp_dir().join(format!("mizpah-agent-cursor-{}", std::process::id()));
        let config = home.join("cfg");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(home.join(".cursor")).unwrap();
        let old_home = std::env::var_os("HOME");
        let old_config = std::env::var_os("MIZPAH_CONFIG_DIR");
        std::env::set_var("HOME", &home);
        std::env::set_var("MIZPAH_CONFIG_DIR", &config);
        let cursor_path = home.join(".cursor/hooks.json");

        run_attach_source_at(
            HookSource::Cursor,
            Some("my-cursor".into()),
            host,
            port,
            Some(&cursor_path),
            None,
        )
        .await
        .unwrap();
        let content = read_file_or_empty(&cursor_path).unwrap();
        assert!(is_managed_command(&content, HookSource::Cursor));
        let state = load_state().unwrap();
        assert!(state.cursor.as_ref().is_some_and(|s| s.enabled));
        assert_eq!(state.cursor.as_ref().unwrap().service, "my-cursor");

        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match old_config {
            Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
            None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn run_attach_source_claude_idempotent() {
        let (hub_url, _store) = crate::test_support::spawn_test_hub().await;
        let url = url::Url::parse(&hub_url).unwrap();
        let host = url.host_str().unwrap_or("127.0.0.1").to_string();
        let port = url.port().unwrap_or(80);

        let _guard = env_lock();
        let home = std::env::temp_dir().join(format!("mizpah-agent-claude-{}", std::process::id()));
        let config = home.join("cfg");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let old_home = std::env::var_os("HOME");
        let old_config = std::env::var_os("MIZPAH_CONFIG_DIR");
        std::env::set_var("HOME", &home);
        std::env::set_var("MIZPAH_CONFIG_DIR", &config);
        let claude_path = home.join(".claude/settings.json");

        run_attach_source_at(
            HookSource::Claude,
            None,
            host.clone(),
            port,
            None,
            Some(&claude_path),
        )
        .await
        .unwrap();
        let once = std::fs::read_to_string(&claude_path).unwrap();
        run_attach_source_at(
            HookSource::Claude,
            None,
            host,
            port,
            None,
            Some(&claude_path),
        )
        .await
        .unwrap();
        let twice = std::fs::read_to_string(&claude_path).unwrap();
        assert_eq!(once, twice);

        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match old_config {
            Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
            None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn run_detach_source_cursor_removes_hooks() {
        with_agent_home(|home, _config| {
            let cursor_path = home.join(".cursor/hooks.json");
            let cmd = managed_command(Path::new("/bin/mzp"), HookSource::Cursor);
            let (merged, _) = super::cursor::merge_cursor_hooks("", &cmd).unwrap();
            std::fs::write(&cursor_path, merged).unwrap();
            run_detach_source_at(HookSource::Cursor, Some(&cursor_path), None).unwrap();
            let content = std::fs::read_to_string(&cursor_path).unwrap();
            assert!(!content.contains("__hook-forward --source cursor"));
            let state = load_state().unwrap();
            assert!(state.cursor.as_ref().is_some_and(|s| !s.enabled));
        });
    }

    #[test]
    fn run_detach_source_empty_config_is_noop() {
        with_agent_home(|home, _config| {
            let cursor_path = home.join(".cursor/hooks.json");
            run_detach_source_at(HookSource::Cursor, Some(&cursor_path), None).unwrap();
        });
    }

    #[test]
    fn run_detach_source_claude_empty_after_remove_writes_minimal() {
        with_agent_home(|home, _config| {
            let claude_path = home.join(".claude/settings.json");
            let cmd = managed_command(Path::new("/bin/mzp"), HookSource::Claude);
            let (merged, _) = super::claude::merge_claude_hooks("", &cmd).unwrap();
            std::fs::write(&claude_path, merged).unwrap();
            run_detach_source_at(HookSource::Claude, None, Some(&claude_path)).unwrap();
            let content = std::fs::read_to_string(&claude_path).unwrap();
            assert_eq!(content.trim(), "{}");
        });
    }

    #[test]
    fn run_detach_all_disables_everything() {
        with_agent_home(|home, config| {
            save_state(&AgentHooksState {
                cursor: Some(SourceState {
                    enabled: true,
                    host: "127.0.0.1".into(),
                    port: 3149,
                    service: "cursor".into(),
                }),
                claude: Some(SourceState {
                    enabled: true,
                    host: "127.0.0.1".into(),
                    port: 3149,
                    service: "claude".into(),
                }),
            })
            .unwrap();
            crate::shell_attach::save_state(&crate::shell_attach::AttachState {
                enabled: true,
                service: None,
                host: "127.0.0.1".into(),
                port: 3149,
            })
            .unwrap();
            let cursor_path = home.join(".cursor/hooks.json");
            std::fs::write(&cursor_path, "{}\n").unwrap();
            run_detach_all().unwrap();
            assert!(!load_state()
                .unwrap()
                .cursor
                .as_ref()
                .is_some_and(|s| s.enabled));
            assert!(!crate::shell_attach::load_state().unwrap().enabled);
            let _ = config;
        });
    }

    #[test]
    fn run_detach_source_reports_missing_managed_hooks() {
        with_agent_home(|home, _config| {
            let cursor_path = home.join(".cursor/hooks.json");
            std::fs::write(
                &cursor_path,
                r#"{"version":1,"hooks":{"stop":[{"command":"./user.sh"}]}}"#,
            )
            .unwrap();
            run_detach_source_at(HookSource::Cursor, Some(&cursor_path), None).unwrap();
            let content = std::fs::read_to_string(&cursor_path).unwrap();
            assert!(content.contains("user.sh"));
        });
    }

    #[tokio::test]
    async fn run_attach_source_reports_already_present() {
        let (hub_url, _store) = crate::test_support::spawn_test_hub().await;
        let url = url::Url::parse(&hub_url).unwrap();
        let host = url.host_str().unwrap_or("127.0.0.1").to_string();
        let port = url.port().unwrap_or(80);

        let _guard = env_lock();
        let home =
            std::env::temp_dir().join(format!("mizpah-agent-present-{}", std::process::id()));
        let config = home.join("cfg");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(home.join(".cursor")).unwrap();
        let old_home = std::env::var_os("HOME");
        let old_config = std::env::var_os("MIZPAH_CONFIG_DIR");
        std::env::set_var("HOME", &home);
        std::env::set_var("MIZPAH_CONFIG_DIR", &config);
        let cursor_path = home.join(".cursor/hooks.json");
        let cmd = managed_command(Path::new("/bin/mzp"), HookSource::Cursor);
        let (merged, _) = super::cursor::merge_cursor_hooks("", &cmd).unwrap();
        std::fs::write(&cursor_path, merged).unwrap();

        run_attach_source_at(
            HookSource::Cursor,
            None,
            host,
            port,
            Some(&cursor_path),
            None,
        )
        .await
        .unwrap();

        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match old_config {
            Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
            None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn run_attach_source_invalid_cursor_json_errors() {
        let (hub_url, _store) = crate::test_support::spawn_test_hub().await;
        let url = url::Url::parse(&hub_url).unwrap();
        let host = url.host_str().unwrap_or("127.0.0.1").to_string();
        let port = url.port().unwrap_or(80);

        let _guard = env_lock();
        let home = std::env::temp_dir().join(format!("mizpah-agent-bad-{}", std::process::id()));
        let config = home.join("cfg");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(home.join(".cursor")).unwrap();
        let old_home = std::env::var_os("HOME");
        let old_config = std::env::var_os("MIZPAH_CONFIG_DIR");
        std::env::set_var("HOME", &home);
        std::env::set_var("MIZPAH_CONFIG_DIR", &config);
        let cursor_path = home.join(".cursor/hooks.json");
        std::fs::write(&cursor_path, "{bad-json").unwrap();

        let err = run_attach_source_at(
            HookSource::Cursor,
            None,
            host,
            port,
            Some(&cursor_path),
            None,
        )
        .await
        .unwrap_err();
        assert!(err.contains("invalid Cursor hooks.json"));

        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match old_config {
            Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
            None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn run_detach_source_creates_disabled_state_when_missing() {
        with_agent_home(|_home, _config| {
            run_detach_source_at(HookSource::Claude, None, None).unwrap();
            let state = load_state().unwrap();
            assert!(state.claude.as_ref().is_some_and(|s| !s.enabled));
        });
    }

    #[test]
    fn run_detach_source_claude_keeps_nonempty_config() {
        with_agent_home(|home, _config| {
            let claude_path = home.join(".claude/settings.json");
            std::fs::write(
                &claude_path,
                r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"user.sh"}]}]}}"#,
            )
            .unwrap();
            run_detach_source_at(HookSource::Claude, None, Some(&claude_path)).unwrap();
            let content = std::fs::read_to_string(&claude_path).unwrap();
            assert!(content.contains("user.sh"));
        });
    }

    #[tokio::test]
    async fn run_attach_claude_wrapper() {
        let (hub_url, _store) = crate::test_support::spawn_test_hub().await;
        let url = url::Url::parse(&hub_url).unwrap();
        let host = url.host_str().unwrap_or("127.0.0.1").to_string();
        let port = url.port().unwrap_or(80);

        let _guard = env_lock();
        let home = std::env::temp_dir().join(format!("mizpah-claude-wrap-{}", std::process::id()));
        let config = home.join("cfg");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let old_home = std::env::var_os("HOME");
        let old_config = std::env::var_os("MIZPAH_CONFIG_DIR");
        std::env::set_var("HOME", &home);
        std::env::set_var("MIZPAH_CONFIG_DIR", &config);

        super::run_attach_claude(Some("svc".into()), host, port)
            .await
            .unwrap();
        super::run_detach_claude().unwrap();

        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match old_config {
            Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
            None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn run_attach_cursor_wrapper() {
        let (hub_url, _store) = crate::test_support::spawn_test_hub().await;
        let url = url::Url::parse(&hub_url).unwrap();
        let host = url.host_str().unwrap_or("127.0.0.1").to_string();
        let port = url.port().unwrap_or(80);

        let _guard = env_lock();
        let home = std::env::temp_dir().join(format!("mizpah-cursor-wrap-{}", std::process::id()));
        let config = home.join("cfg");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(home.join(".cursor")).unwrap();
        let old_home = std::env::var_os("HOME");
        let old_config = std::env::var_os("MIZPAH_CONFIG_DIR");
        std::env::set_var("HOME", &home);
        std::env::set_var("MIZPAH_CONFIG_DIR", &config);

        super::run_attach_cursor(None, host, port).await.unwrap();
        super::run_detach_cursor().unwrap();

        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match old_config {
            Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
            None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn run_attach_source_default_service() {
        let (hub_url, _store) = crate::test_support::spawn_test_hub().await;
        let url = url::Url::parse(&hub_url).unwrap();
        let host = url.host_str().unwrap_or("127.0.0.1").to_string();
        let port = url.port().unwrap_or(80);

        let _guard = env_lock();
        let home = std::env::temp_dir().join(format!("mizpah-default-svc-{}", std::process::id()));
        let config = home.join("cfg");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let old_home = std::env::var_os("HOME");
        let old_config = std::env::var_os("MIZPAH_CONFIG_DIR");
        std::env::set_var("HOME", &home);
        std::env::set_var("MIZPAH_CONFIG_DIR", &config);

        run_attach_source_at(
            HookSource::Claude,
            Some("   ".into()),
            host,
            port,
            None,
            Some(&home.join(".claude/settings.json")),
        )
        .await
        .unwrap();
        assert_eq!(load_state().unwrap().claude.unwrap().service, "claude");

        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match old_config {
            Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
            None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn run_detach_source_missing_claude_config() {
        with_agent_home(|_home, _config| {
            run_detach_source_at(HookSource::Claude, None, None).unwrap();
        });
    }

    #[test]
    fn run_detach_all_with_claude_config() {
        with_agent_home(|home, config| {
            let claude_path = home.join(".claude/settings.json");
            let cmd = managed_command(Path::new("/bin/mzp"), HookSource::Claude);
            let (merged, _) = super::claude::merge_claude_hooks("", &cmd).unwrap();
            std::fs::write(&claude_path, merged).unwrap();
            save_state(&AgentHooksState {
                claude: Some(SourceState {
                    enabled: true,
                    host: "127.0.0.1".into(),
                    port: 3149,
                    service: "claude".into(),
                }),
                cursor: None,
            })
            .unwrap();
            run_detach_all().unwrap();
            assert!(!load_state()
                .unwrap()
                .claude
                .as_ref()
                .is_some_and(|s| s.enabled));
            let _ = config;
        });
    }
}
