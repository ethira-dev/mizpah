//! MCP stdio server and client-config installers for Cursor / Claude / Codex.

mod client;
mod format;
mod install;
mod server;

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
