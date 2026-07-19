//! Shell attach state, startup-file hooks, and init script generation.

mod hooks;
mod state;

pub(crate) use hooks::{install_shell_hooks, run_shell_init};
pub use state::AttachState;
pub(crate) use state::{load_state, save_state};

pub use crate::hub::{DEFAULT_HOST, DEFAULT_PORT};

/// Run `mzp attach`.
pub async fn run_attach(service: Option<String>, host: String, port: u16) -> Result<(), String> {
    let bin =
        crate::mcp::resolve_binary_path().map_err(|e| format!("could not resolve binary: {e}"))?;

    let service = service
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let touched = install_shell_hooks(&bin)?;
    crate::hub::ensure_hub(&host, port, None, false)
        .await
        .map_err(|e| e.to_string())?;

    let state = AttachState {
        enabled: true,
        service,
        host: host.clone(),
        port,
    };
    save_state(&state).map_err(|e| format!("failed to save attach state: {e}"))?;

    let url = crate::hub::hub_url(&host, port);
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
    if !crate::hub::probe_hub(&host, port).await {
        return Err(format!(
            "hub at {} is not reachable\n\
             hint: run `mzp attach` or pipe logs into `mzp` first",
            crate::hub::hub_url(&host, port)
        ));
    }
    let url = crate::hub::hub_url(&host, port);
    open::that(&url).map_err(|e| format!("failed to open browser: {e}"))?;
    eprintln!("opened {url}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell_attach::state::load_state_from;
    use crate::test_support::env_lock;

    fn with_home_and_config<F: FnOnce(&std::path::Path, &std::path::Path)>(
        f: F,
    ) {
        let _guard = env_lock();
        let home = std::env::temp_dir().join(format!(
            "mizpah-attach-home-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config = home.join("config");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&config).unwrap();
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
    fn resolve_open_target_flags_override_defaults() {
        with_home_and_config(|_home, _config| {
            save_state(&AttachState {
                enabled: true,
                service: None,
                host: "10.0.0.5".into(),
                port: 4000,
            })
            .unwrap();
            let (h, p) = resolve_open_target(Some("127.0.0.1".into()), Some(3149)).unwrap();
            assert_eq!(h, "127.0.0.1");
            assert_eq!(p, 3149);

            let (h, p) = resolve_open_target(Some("10.0.0.1".into()), Some(9999)).unwrap();
            assert_eq!(h, "10.0.0.1");
            assert_eq!(p, 9999);
        });
    }

    #[test]
    fn resolve_open_target_from_saved_state() {
        with_home_and_config(|_home, config| {
            save_state(&AttachState {
                enabled: true,
                service: Some("api".into()),
                host: "10.0.0.5".into(),
                port: 4000,
            })
            .unwrap();
            let (h, p) = resolve_open_target(None, None).unwrap();
            assert_eq!(h, "10.0.0.5");
            assert_eq!(p, 4000);
            let _ = config;
        });
    }

    #[test]
    fn resolve_open_target_empty_host_uses_default() {
        with_home_and_config(|_home, _config| {
            save_state(&AttachState {
                enabled: true,
                service: None,
                host: String::new(),
                port: 0,
            })
            .unwrap();
            let (h, p) = resolve_open_target(None, None).unwrap();
            assert_eq!(h, DEFAULT_HOST);
            assert_eq!(p, DEFAULT_PORT);
        });
    }

    #[test]
    fn run_detach_disables_enabled_state() {
        with_home_and_config(|_home, _config| {
            save_state(&AttachState {
                enabled: true,
                service: None,
                host: DEFAULT_HOST.into(),
                port: DEFAULT_PORT,
            })
            .unwrap();
            run_detach().unwrap();
            let state = load_state().unwrap();
            assert!(!state.enabled);
        });
    }

    #[test]
    fn run_detach_idempotent_when_already_disabled() {
        with_home_and_config(|_home, _config| {
            run_detach().unwrap();
            run_detach().unwrap();
            assert!(!load_state().unwrap().enabled);
        });
    }

    #[tokio::test]
    async fn run_attach_enables_state_and_installs_hooks() {
        let (hub_url, _store) = crate::test_support::spawn_test_hub().await;
        let url = url::Url::parse(&hub_url).unwrap();
        let host = url.host_str().unwrap_or("127.0.0.1").to_string();
        let port = url.port().unwrap_or(80);

        let _guard = env_lock();
        let home = std::env::temp_dir().join(format!(
            "mizpah-attach-run-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config = home.join("config");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&config).unwrap();
        let old_home = std::env::var_os("HOME");
        let old_config = std::env::var_os("MIZPAH_CONFIG_DIR");
        std::env::set_var("HOME", &home);
        std::env::set_var("MIZPAH_CONFIG_DIR", &config);

        run_attach(Some("my-svc".into()), host.clone(), port)
            .await
            .unwrap();
        let state = load_state().unwrap();
        assert!(state.enabled);
        assert_eq!(state.service.as_deref(), Some("my-svc"));
        assert_eq!(state.host, host);
        assert_eq!(state.port, port);

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
    async fn run_open_unreachable_hub_errors() {
        let err = run_open("127.0.0.1".into(), 19996).await.unwrap_err();
        assert!(err.contains("not reachable"));
    }

    #[tokio::test]
    async fn run_open_succeeds_when_hub_up() {
        let (hub_url, _store) = crate::test_support::spawn_test_hub().await;
        let url = url::Url::parse(&hub_url).unwrap();
        let host = url.host_str().unwrap_or("127.0.0.1").to_string();
        let port = url.port().unwrap_or(80);
        run_open(host, port).await.unwrap();
    }

    #[test]
    fn resolve_open_target_empty_host_flag_uses_state() {
        with_home_and_config(|_home, config| {
            save_state(&AttachState {
                enabled: true,
                service: None,
                host: "192.168.1.1".into(),
                port: 9999,
            })
            .unwrap();
            assert_eq!(load_state_from(&config.join("attach.json")).unwrap().port, 9999);
            let (h, p) = resolve_open_target(Some(String::new()), None).unwrap();
            assert_eq!(h, "192.168.1.1");
            assert_eq!(p, 9999);
        });
    }
}
