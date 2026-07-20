//! `mzp setup` and `mzp doctor` — one-shot agent readiness.

use crate::hub;
use crate::mcp;
use crate::mcp::{discover_clients, ClientKind};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const SKILL_CMD: &str = "npx skills add ethira-dev/mizpah";

#[derive(Debug, Clone)]
pub struct SetupOpts {
    pub host: String,
    pub port: u16,
    pub project: Option<PathBuf>,
    pub with_skill: bool,
    pub skip_mcp_install: bool,
}

/// Ensure hub, install MCP configs, print next steps.
pub async fn run_setup(opts: SetupOpts) -> i32 {
    eprintln!("mizpah setup");
    eprintln!("  hub: {}:{}", opts.host, opts.port);

    match hub::ensure_hub(&opts.host, opts.port, opts.project.as_deref(), false).await {
        Ok(()) => eprintln!("  hub: ready"),
        Err(e) => {
            eprintln!("  hub: failed — {e}");
            return 1;
        }
    }

    if !opts.skip_mcp_install {
        let code = mcp::run_install();
        if code != 0 {
            eprintln!("  mcp: install reported errors (see above)");
        }
    } else {
        eprintln!("  mcp: skipped (--skip-mcp-install)");
    }

    if hub::probe_hub(&opts.host, opts.port).await {
        if let Ok(stats) = mcp::HubClient::new(hub::hub_url(&opts.host, opts.port))
            .get_stats()
            .await
        {
            eprintln!(
                "  stats: {} entries, ~{} bytes",
                stats.get("count").and_then(|v| v.as_u64()).unwrap_or(0),
                stats
                    .get("approxBytes")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
            );
        }
    }

    if opts.with_skill {
        match run_skill_install() {
            Ok(()) => eprintln!("  skill: installed via npx"),
            Err(e) => eprintln!("  skill: {e}"),
        }
    } else {
        eprintln!("  skill: run `{SKILL_CMD}` (or `mzp setup --with-skill`)");
    }

    eprintln!();
    eprintln!("Next: restart Cursor / Claude / Codex, then ask about your logs.");
    eprintln!("Pipe logs with:  my-app 2>&1 | mzp");
    eprintln!("Or wrap a command:  mzp run -- npm test");
    0
}

fn run_skill_install() -> Result<(), String> {
    let status = Command::new("npx")
        .args(["skills", "add", "ethira-dev/mizpah"])
        .status()
        .map_err(|e| format!("could not run npx: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("npx exited with {status}"))
    }
}

#[derive(Debug)]
pub struct DoctorCheck {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

/// Read-only readiness checks.
pub async fn run_doctor(host: &str, port: u16) -> i32 {
    let checks = collect_doctor_checks(host, port).await;
    let mut failed = 0usize;
    eprintln!("mizpah doctor");
    for c in &checks {
        let mark = if c.ok { "ok" } else { "!!" };
        if !c.ok {
            failed += 1;
        }
        eprintln!("  [{mark}] {}: {}", c.name, c.detail);
    }
    if failed == 0 {
        eprintln!("All checks passed.");
        0
    } else {
        eprintln!("{failed} check(s) need attention. Try `mzp setup`.");
        1
    }
}

pub async fn collect_doctor_checks(host: &str, port: u16) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    match mcp::resolve_binary_path() {
        Ok(p) => checks.push(DoctorCheck {
            name: "binary",
            ok: true,
            detail: p.display().to_string(),
        }),
        Err(e) => checks.push(DoctorCheck {
            name: "binary",
            ok: false,
            detail: e.to_string(),
        }),
    }

    let hub_up = hub::probe_hub(host, port).await;
    checks.push(DoctorCheck {
        name: "hub",
        ok: hub_up,
        detail: if hub_up {
            format!("reachable at http://{host}:{port}")
        } else {
            format!("not reachable at http://{host}:{port} — run `mzp setup` or `mzp hub start`")
        },
    });

    let clients = discover_clients();
    let mut registered = 0usize;
    let mut product_dirs = 0usize;
    for (kind, path_opt) in &clients {
        let Some(path) = path_opt else {
            continue;
        };
        product_dirs += 1;
        if path.exists() {
            if let Ok(text) = std::fs::read_to_string(path) {
                let has = match kind {
                    ClientKind::Codex => {
                        text.contains("[mcp_servers.mizpah]") || text.contains("mizpah")
                    }
                    _ => text.contains("\"mizpah\""),
                };
                if has {
                    registered += 1;
                }
            }
        }
    }
    checks.push(DoctorCheck {
        name: "mcp-config",
        ok: registered > 0 || product_dirs == 0,
        detail: if product_dirs == 0 {
            "no AI client product dirs found yet".into()
        } else {
            format!("{registered}/{product_dirs} client config(s) mention mizpah")
        },
    });

    let npx_ok = Command::new("npx")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    checks.push(DoctorCheck {
        name: "npx",
        ok: npx_ok,
        detail: if npx_ok {
            format!("available (for `{SKILL_CMD}`)")
        } else {
            "npx not found — install Node to add the agent skill".into()
        },
    });

    let path_has_mzp = which_in_path("mzp") || which_in_path("mizpah");
    checks.push(DoctorCheck {
        name: "path",
        ok: true,
        detail: if path_has_mzp {
            "mzp/mizpah found on PATH".into()
        } else {
            "mzp/mizpah not on PATH (ok if you invoke via absolute path)".into()
        },
    });

    checks
}

fn which_in_path(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_cmd_constant() {
        assert!(SKILL_CMD.contains("ethira-dev/mizpah"));
    }
}
