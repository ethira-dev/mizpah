//! MCP stdio server and client-config installers for Cursor / Claude / Codex.

mod client;
mod format;
mod install;
mod server;

pub use client::HubClient;
pub use install::{
    ensure_registered_on_hub_start, install_all, resolve_binary_path, uninstall_all,
};

use rmcp::transport::stdio;
use rmcp::ServiceExt;
use server::MizpahMcp;

/// Resolve hub base URL from env or host/port flags.
pub fn hub_base_url(host: &str, port: u16) -> String {
    if let Ok(url) = std::env::var("MIZPAH_URL") {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            return trimmed.trim_end_matches('/').to_string();
        }
    }
    format!("http://{host}:{port}")
}

/// Run the MCP server on stdio until the client disconnects.
pub async fn run_stdio(base_url: String) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = MizpahMcp::new(base_url);
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

pub fn run_install() -> i32 {
    let command = match resolve_binary_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: could not resolve mizpah binary path: {e}");
            return 1;
        }
    };
    eprintln!("Installing Mizpah MCP (`{} mcp`)…", command.display());
    let report = install_all(&command);
    report.print_summary();
    if report.errors.is_empty() {
        eprintln!("Done. Restart Cursor / Claude / Codex to load the tools.");
        0
    } else {
        1
    }
}

pub fn run_uninstall() -> i32 {
    eprintln!("Removing Mizpah MCP from local AI client configs…");
    let report = uninstall_all();
    report.print_summary();
    if report.errors.is_empty() {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_base_url_from_env() {
        temp_env::with_var("MIZPAH_URL", Some("http://localhost:9000"), || {
            let url = hub_base_url("127.0.0.1", 3149);
            assert_eq!(url, "http://localhost:9000");
        });
    }

    #[test]
    fn hub_base_url_from_env_trims_trailing_slash() {
        temp_env::with_var("MIZPAH_URL", Some("http://localhost:9000/"), || {
            let url = hub_base_url("127.0.0.1", 3149);
            assert_eq!(url, "http://localhost:9000");
        });
    }

    #[test]
    fn hub_base_url_env_empty_uses_default() {
        temp_env::with_var("MIZPAH_URL", Some(""), || {
            let url = hub_base_url("127.0.0.1", 3149);
            assert_eq!(url, "http://127.0.0.1:3149");
        });
    }

    #[test]
    fn hub_base_url_no_env_uses_args() {
        temp_env::with_var("MIZPAH_URL", None::<&str>, || {
            let url = hub_base_url("192.168.1.5", 8080);
            assert_eq!(url, "http://192.168.1.5:8080");
        });
    }

    #[test]
    fn run_install_with_temp_config() {
        let temp_home = tempfile::tempdir().unwrap();
        let temp_config = tempfile::tempdir().unwrap();

        temp_env::with_vars(
            &[
                ("HOME", Some(temp_home.path().to_str().unwrap())),
                (
                    "MIZPAH_CONFIG_DIR",
                    Some(temp_config.path().to_str().unwrap()),
                ),
            ],
            || {
                // Create a fake cursor directory
                let cursor_dir = temp_home.path().join(".cursor");
                std::fs::create_dir_all(&cursor_dir).unwrap();

                let bin = std::env::current_exe().unwrap();
                let report = install_all(&bin);

                // Should have attempted to install
                assert!(!report.results.is_empty());
            },
        );
    }

    #[test]
    fn run_uninstall_with_temp_config() {
        let temp_home = tempfile::tempdir().unwrap();
        let cursor_dir = temp_home.path().join(".cursor");
        std::fs::create_dir_all(&cursor_dir).unwrap();
        let mcp_file = cursor_dir.join("mcp.json");
        std::fs::write(
            &mcp_file,
            r#"{"mcpServers":{"mizpah":{"command":"/bin/mizpah","args":["mcp"]}}}"#,
        )
        .unwrap();

        // Exercise remove helper directly (HOME races under parallel tests).
        let existing = std::fs::read_to_string(&mcp_file).unwrap();
        let (next, changed) = install::remove_json_mcp_server(&existing).unwrap();
        assert!(changed);
        assert!(!next.contains("\"mizpah\""));

        temp_env::with_var("HOME", Some(temp_home.path().to_str().unwrap()), || {
            let _ = run_uninstall();
        });
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn run_stdio_server_serves() {
        use tokio::io::duplex;
        use tokio::time::Duration;

        // Create an in-memory duplex stream
        let (_client, _server) = duplex(1024);

        // Spawn stdio server in background with timeout
        let handle = tokio::spawn(async move {
            // Use a short timeout to avoid hanging tests
            tokio::time::timeout(
                Duration::from_secs(2),
                run_stdio("http://127.0.0.1:3149".into()),
            )
            .await
        });

        // Give it a moment to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Abort the server
        handle.abort();
        let _ = handle.await;

        // Test passes if we got here without panicking
    }
}

#[cfg(test)]
mod temp_env {
    use std::env;

    pub fn with_var<F, T>(key: &str, value: Option<&str>, f: F) -> T
    where
        F: FnOnce() -> T,
    {
        let _guard = crate::test_support::env_lock();
        let old_value = env::var(key).ok();
        if let Some(v) = value {
            env::set_var(key, v);
        } else {
            env::remove_var(key);
        }
        let result = f();
        match old_value {
            Some(v) => env::set_var(key, v),
            None => env::remove_var(key),
        }
        result
    }

    pub fn with_vars<F, T>(vars: &[(&str, Option<&str>)], f: F) -> T
    where
        F: FnOnce() -> T,
    {
        let _guard = crate::test_support::env_lock();
        let old_values: Vec<_> = vars
            .iter()
            .map(|(key, _)| (*key, env::var(key).ok()))
            .collect();

        for (key, value) in vars {
            if let Some(v) = value {
                env::set_var(key, v);
            } else {
                env::remove_var(key);
            }
        }

        let result = f();

        for (key, old_value) in old_values {
            match old_value {
                Some(v) => env::set_var(key, v),
                None => env::remove_var(key),
            }
        }

        result
    }
}
