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

    #[test]
    fn resolve_open_target_flags_override_defaults() {
        let (h, p) = resolve_open_target(Some("127.0.0.1".into()), Some(3149)).unwrap();
        assert_eq!(h, "127.0.0.1");
        assert_eq!(p, 3149);

        let (h, p) = resolve_open_target(Some("10.0.0.1".into()), Some(9999)).unwrap();
        assert_eq!(h, "10.0.0.1");
        assert_eq!(p, 9999);
    }
}
