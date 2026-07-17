//! Clap CLI definition and command dispatch.

use crate::agent_hooks;
use crate::browser_attach;
use crate::hub;
use crate::mcp;
use crate::shell_attach;
use crate::shell_forward;
use crate::store::DEFAULT_MAX_BYTES;
use crate::update;
use crate::{ensure_bind_allowed, init_tracing_stderr, run_pipe_mode};
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use tracing::error;

#[derive(Debug, Clone, Args)]
pub struct HubArgs {
    /// Host to bind (hub) or connect to (attach)
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    /// Port to bind (hub) or connect to (attach)
    #[arg(short, long, default_value_t = hub::DEFAULT_PORT)]
    pub port: u16,
}

#[derive(Debug, Clone, Args)]
pub struct HubGlobalArgs {
    /// Hub host
    #[arg(long, default_value = "127.0.0.1", global = true)]
    pub host: String,

    /// Hub port
    #[arg(short, long, default_value_t = hub::DEFAULT_PORT, global = true)]
    pub port: u16,
}

/// Shared flags for `attach browser` and `browser attach`.
#[derive(Debug, Clone, Args)]
pub struct BrowserAttachArgs {
    /// Shared service name (default: page host, e.g. localhost:5173)
    #[arg(short, long)]
    pub service: Option<String>,

    #[command(flatten)]
    pub hub: HubArgs,

    /// Chrome remote-debugging port
    #[arg(long, default_value_t = 9222)]
    pub cdp_port: u16,

    /// CDP browser websocket URL (overrides --cdp-port)
    #[arg(long)]
    pub cdp_url: Option<String>,

    /// Launch Chrome/Edge with a dedicated Mizpah profile and debugging enabled
    #[arg(long, default_value_t = false)]
    pub launch: bool,

    /// Also ingest Image/Font/Media/Stylesheet network metadata (no bodies)
    #[arg(long, default_value_t = false)]
    pub all_network: bool,
}

impl BrowserAttachArgs {
    pub fn into_opts(self) -> browser_attach::BrowserAttachOpts {
        browser_attach::BrowserAttachOpts {
            service: self.service,
            host: self.hub.host,
            port: self.hub.port,
            cdp_port: self.cdp_port,
            cdp_url: self.cdp_url,
            launch: self.launch,
            all_network: self.all_network,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    about = "JSON log viewer — pipe logs and inspect them in a web UI",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Service name for this ingest stream (defaults to absolute cwd)
    #[arg(short, long, global = false)]
    pub service: Option<String>,

    #[command(flatten)]
    pub hub: HubArgs,

    /// Max in-memory log bytes (hub only)
    #[arg(long, default_value_t = DEFAULT_MAX_BYTES)]
    pub max_bytes: u64,

    /// Do not open the browser when starting as hub
    #[arg(long, default_value_t = false)]
    pub no_open: bool,

    /// Project directory for "Check with Claude/Cursor" agent sessions (hub only)
    #[arg(long, env = "MIZPAH_PROJECT")]
    pub project: Option<PathBuf>,

    /// Allow binding the hub on a non-loopback address (unauthenticated ingest).
    /// Without this flag, only 127.0.0.1 / ::1 / localhost are accepted.
    #[arg(long, default_value_t = false)]
    pub allow_remote: bool,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// MCP server for Cursor / Claude / Codex (queries the live hub)
    Mcp {
        #[command(subcommand)]
        action: Option<McpAction>,

        #[command(flatten)]
        hub: HubArgs,
    },
    /// Attach a log source (shell, browser, cursor, or claude)
    Attach {
        #[command(subcommand)]
        target: Option<AttachTarget>,

        /// Shared service name (shell when no subcommand; default: absolute cwd per command)
        #[arg(short, long)]
        service: Option<String>,

        #[command(flatten)]
        hub: HubArgs,
    },
    /// Detach a log source (shell, cursor, claude, or all). Hub stays up.
    Detach {
        /// Target to detach (default: shell)
        #[arg(value_enum, default_value_t = DetachTarget::Shell)]
        target: DetachTarget,
    },
    /// Capture Chrome/Edge console + network via CDP into Mizpah
    Browser {
        #[command(subcommand)]
        action: BrowserAction,
    },
    /// Start, stop, or restart the background hub
    Hub {
        #[command(subcommand)]
        action: HubAction,

        #[command(flatten)]
        hub: HubGlobalArgs,

        /// Project directory for "Check with Claude/Cursor" (start/restart only)
        #[arg(long, env = "MIZPAH_PROJECT", global = true)]
        project: Option<PathBuf>,

        /// Allow binding on a non-loopback address (unauthenticated ingest)
        #[arg(long, default_value_t = false, global = true)]
        allow_remote: bool,
    },
    /// Open the Mizpah web UI in a browser
    Open {
        /// Hub host (defaults to attach state, then 127.0.0.1)
        #[arg(long)]
        host: Option<String>,

        /// Hub port (defaults to attach state, then 3149)
        #[arg(short, long)]
        port: Option<u16>,
    },
    /// Print shell init snippet for rc files (internal)
    #[command(name = "__shell-init", hide = true)]
    ShellInit {
        /// Shell kind: zsh or bash
        shell: String,
    },
    /// Forward stdin lines to the hub (internal; used by shell hooks)
    #[command(name = "__shell-forward", hide = true)]
    ShellForward {
        /// Initial service fallback (absolute cwd) until a per-command control frame arrives
        #[arg(long)]
        tty_service: String,
    },
    /// Forward Cursor/Claude hook JSON from stdin to the hub (internal)
    #[command(name = "__hook-forward", hide = true)]
    HookForward {
        /// Hook source: cursor or claude
        #[arg(long)]
        source: String,
    },
    /// Wait for parent exit then start hub (internal; used after self-update)
    #[command(name = "update-resume", hide = true)]
    UpdateResume {
        /// Parent hub PID to wait for
        #[arg(long)]
        wait_pid: u32,

        #[command(flatten)]
        hub: HubArgs,

        /// Project directory for agent sessions
        #[arg(long)]
        project: PathBuf,

        /// Max in-memory log bytes
        #[arg(long, default_value_t = DEFAULT_MAX_BYTES)]
        max_bytes: u64,
    },
}

#[derive(Debug, Subcommand)]
pub enum AttachTarget {
    /// Capture stdout/stderr from new interactive shells
    Shell {
        /// Shared service name for all hooked shells (default: absolute cwd per command)
        #[arg(short, long)]
        service: Option<String>,

        #[command(flatten)]
        hub: HubArgs,
    },
    /// Capture Chrome/Edge console + network via CDP
    Browser {
        #[command(flatten)]
        args: BrowserAttachArgs,
    },
    /// Install Cursor agent hooks that forward lifecycle events into the hub
    Cursor {
        /// Hub service name (default: cursor)
        #[arg(short, long)]
        service: Option<String>,

        #[command(flatten)]
        hub: HubArgs,
    },
    /// Install Claude Code hooks that forward lifecycle events into the hub
    Claude {
        /// Hub service name (default: claude)
        #[arg(short, long)]
        service: Option<String>,

        #[command(flatten)]
        hub: HubArgs,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DetachTarget {
    /// Disable shell stdout/stderr capture
    Shell,
    /// Remove Mizpah-managed Cursor hooks
    Cursor,
    /// Remove Mizpah-managed Claude Code hooks
    Claude,
    /// Detach shell + cursor + claude
    All,
}

#[derive(Debug, Subcommand)]
pub enum McpAction {
    /// Register Mizpah in Cursor, Claude Desktop, Claude Code, and Codex configs
    Install,
    /// Remove Mizpah MCP entries from those configs
    Uninstall,
}

#[derive(Debug, Subcommand)]
pub enum HubAction {
    /// Start a detached hub if one is not already healthy
    Start,
    /// Stop the hub tracked by the PID file for this port
    Stop,
    /// Stop then start the hub (clears the in-memory buffer)
    Restart,
}

#[derive(Debug, Subcommand)]
pub enum BrowserAction {
    /// Attach to Chrome/Edge DevTools and forward console + network into the hub
    Attach {
        #[command(flatten)]
        args: BrowserAttachArgs,
    },
}

pub async fn run() {
    crate::util::ensure_rustls_crypto_provider();
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Mcp { action, hub }) => match action {
            None => {
                init_tracing_stderr();

                let base_url = mcp::hub_base_url(&hub.host, hub.port);
                if let Err(err) = mcp::run_stdio(base_url).await {
                    error!(error = %err, "MCP server failed");
                    std::process::exit(1);
                }
            }
            Some(McpAction::Install) => {
                std::process::exit(mcp::run_install());
            }
            Some(McpAction::Uninstall) => {
                std::process::exit(mcp::run_uninstall());
            }
        },
        Some(Commands::Attach {
            target,
            service,
            hub,
        }) => {
            init_tracing_stderr();
            let result = match target {
                None => shell_attach::run_attach(service, hub.host, hub.port).await,
                Some(AttachTarget::Shell { service, hub }) => {
                    shell_attach::run_attach(service, hub.host, hub.port).await
                }
                Some(AttachTarget::Browser { args }) => {
                    browser_attach::run_browser_attach(args.into_opts()).await
                }
                Some(AttachTarget::Cursor { service, hub }) => {
                    agent_hooks::run_attach_cursor(service, hub.host, hub.port).await
                }
                Some(AttachTarget::Claude { service, hub }) => {
                    agent_hooks::run_attach_claude(service, hub.host, hub.port).await
                }
            };
            if let Err(err) = result {
                eprintln!("error: {err}");
                std::process::exit(1);
            }
        }
        Some(Commands::Detach { target }) => {
            init_tracing_stderr();
            let result = match target {
                DetachTarget::Shell => shell_attach::run_detach(),
                DetachTarget::Cursor => agent_hooks::run_detach_cursor(),
                DetachTarget::Claude => agent_hooks::run_detach_claude(),
                DetachTarget::All => agent_hooks::run_detach_all(),
            };
            if let Err(err) = result {
                eprintln!("error: {err}");
                std::process::exit(1);
            }
        }
        Some(Commands::Browser { action }) => {
            // Alias for `mzp attach browser`
            init_tracing_stderr();
            match action {
                BrowserAction::Attach { args } => {
                    if let Err(err) = browser_attach::run_browser_attach(args.into_opts()).await {
                        eprintln!("error: {err}");
                        std::process::exit(1);
                    }
                }
            }
        }
        Some(Commands::Hub {
            action,
            hub,
            project,
            allow_remote,
        }) => {
            init_tracing_stderr();
            if matches!(action, HubAction::Start | HubAction::Restart) {
                ensure_bind_allowed(&hub.host, allow_remote);
            }
            let result = match action {
                HubAction::Start => {
                    hub::run_hub_start(hub.host, hub.port, project, allow_remote).await
                }
                HubAction::Stop => hub::run_hub_stop(hub.host, hub.port).await,
                HubAction::Restart => {
                    hub::run_hub_restart(hub.host, hub.port, project, allow_remote).await
                }
            };
            if let Err(err) = result {
                eprintln!("error: {err}");
                std::process::exit(1);
            }
        }
        Some(Commands::Open { host, port }) => {
            init_tracing_stderr();
            let (host, port) = match shell_attach::resolve_open_target(host, port) {
                Ok(t) => t,
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            };
            if let Err(err) = shell_attach::run_open(host, port).await {
                eprintln!("error: {err}");
                std::process::exit(1);
            }
        }
        Some(Commands::ShellInit { shell }) => {
            // stdout is evaluated by the shell — keep quiet on stderr unless error
            if let Err(err) = shell_attach::run_shell_init(&shell) {
                eprintln!("error: {err}");
                std::process::exit(1);
            }
        }
        Some(Commands::ShellForward { tty_service }) => {
            init_tracing_stderr();
            if let Err(err) = shell_forward::run_shell_forward(tty_service).await {
                eprintln!("error: {err}");
                std::process::exit(1);
            }
        }
        Some(Commands::HookForward { source }) => {
            // stdout must stay empty (Claude injects SessionStart/UserPromptSubmit stdout)
            init_tracing_stderr();
            let Some(src) = agent_hooks::HookSource::parse(&source) else {
                // Fail-open for the agent loop
                std::process::exit(0);
            };
            agent_hooks::run_hook_forward(src).await;
            std::process::exit(0);
        }
        Some(Commands::UpdateResume {
            wait_pid,
            hub,
            project,
            max_bytes,
        }) => {
            init_tracing_stderr();
            if let Err(err) =
                update::run_update_resume(wait_pid, hub.host, hub.port, project, max_bytes).await
            {
                eprintln!("error: {err}");
                std::process::exit(1);
            }
        }
        None => {
            init_tracing_stderr();
            run_pipe_mode(cli).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use std::path::PathBuf;

    #[test]
    fn clap_accepts_mcp_without_service() {
        let cli = Cli::try_parse_from(["mizpah", "mcp"]).expect("mcp should parse");
        match cli.command {
            Some(Commands::Mcp { action: None, .. }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn clap_accepts_mcp_install() {
        let cli = Cli::try_parse_from(["mizpah", "mcp", "install"]).unwrap();
        match cli.command {
            Some(Commands::Mcp {
                action: Some(McpAction::Install),
                ..
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn clap_pipe_mode_still_works() {
        let cli = Cli::try_parse_from(["mizpah", "--service", "api", "--no-open"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.service.as_deref(), Some("api"));
        assert!(cli.no_open);
    }

    #[test]
    fn clap_accepts_project_flag() {
        let cli = Cli::try_parse_from(["mizpah", "--project", "/tmp/my-app", "--no-open"]).unwrap();
        assert_eq!(
            cli.project.as_deref(),
            Some(std::path::Path::new("/tmp/my-app"))
        );
    }

    #[test]
    fn clap_help_renders() {
        let mut cmd = Cli::command();
        let help = cmd.render_help().to_string();
        assert!(help.contains("mcp"));
        assert!(help.contains("attach"));
        assert!(help.contains("detach"));
        assert!(help.contains("browser"));
        assert!(help.contains("hub"));
        assert!(help.contains("open"));
    }

    #[test]
    fn clap_accepts_browser_attach() {
        let attach = Cli::try_parse_from([
            "mizpah",
            "browser",
            "attach",
            "--launch",
            "--cdp-port",
            "9223",
            "--service",
            "web",
            "--all-network",
            "--host",
            "127.0.0.1",
            "-p",
            "3149",
        ])
        .unwrap();
        match attach.command {
            Some(Commands::Browser {
                action:
                    BrowserAction::Attach {
                        args:
                            BrowserAttachArgs {
                                service: Some(s),
                                hub: HubArgs { port: 3149, .. },
                                cdp_port: 9223,
                                launch: true,
                                all_network: true,
                                cdp_url: None,
                            },
                    },
            }) => assert_eq!(s, "web"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn clap_accepts_hub_start_stop_restart() {
        let start = Cli::try_parse_from([
            "mizpah",
            "hub",
            "start",
            "--host",
            "127.0.0.1",
            "-p",
            "3149",
        ])
        .unwrap();
        match start.command {
            Some(Commands::Hub {
                action: HubAction::Start,
                hub: HubGlobalArgs { port: 3149, .. },
                project: None,
                ..
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }

        let stop = Cli::try_parse_from(["mizpah", "hub", "stop", "--port", "9999"]).unwrap();
        match stop.command {
            Some(Commands::Hub {
                action: HubAction::Stop,
                hub: HubGlobalArgs { port: 9999, .. },
                ..
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }

        let restart =
            Cli::try_parse_from(["mizpah", "hub", "restart", "--project", "/tmp/my-app"]).unwrap();
        match restart.command {
            Some(Commands::Hub {
                action: HubAction::Restart,
                project: Some(p),
                ..
            }) => assert_eq!(p, PathBuf::from("/tmp/my-app")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn clap_accepts_attach_detach_open() {
        let attach = Cli::try_parse_from([
            "mizpah",
            "attach",
            "--service",
            "dev",
            "--host",
            "127.0.0.1",
            "-p",
            "3149",
        ])
        .unwrap();
        match attach.command {
            Some(Commands::Attach {
                target: None,
                service: Some(s),
                hub: HubArgs { port: 3149, .. },
                ..
            }) => assert_eq!(s, "dev"),
            other => panic!("unexpected: {other:?}"),
        }

        let detach = Cli::try_parse_from(["mizpah", "detach"]).unwrap();
        assert!(matches!(
            detach.command,
            Some(Commands::Detach {
                target: DetachTarget::Shell
            })
        ));

        let open = Cli::try_parse_from(["mizpah", "open", "--port", "9999"]).unwrap();
        match open.command {
            Some(Commands::Open {
                host: None,
                port: Some(9999),
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn clap_accepts_attach_targets() {
        let shell = Cli::try_parse_from(["mizpah", "attach", "shell", "--service", "dev"]).unwrap();
        match shell.command {
            Some(Commands::Attach {
                target:
                    Some(AttachTarget::Shell {
                        service: Some(s), ..
                    }),
                ..
            }) => assert_eq!(s, "dev"),
            other => panic!("unexpected: {other:?}"),
        }

        let browser = Cli::try_parse_from([
            "mizpah",
            "attach",
            "browser",
            "--launch",
            "--cdp-port",
            "9223",
            "--all-network",
        ])
        .unwrap();
        match browser.command {
            Some(Commands::Attach {
                target:
                    Some(AttachTarget::Browser {
                        args:
                            BrowserAttachArgs {
                                launch: true,
                                cdp_port: 9223,
                                all_network: true,
                                ..
                            },
                    }),
                ..
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }

        let cursor = Cli::try_parse_from(["mizpah", "attach", "cursor", "-p", "3149"]).unwrap();
        assert!(matches!(
            cursor.command,
            Some(Commands::Attach {
                target: Some(AttachTarget::Cursor {
                    hub: HubArgs { port: 3149, .. },
                    ..
                }),
                ..
            })
        ));

        let claude =
            Cli::try_parse_from(["mizpah", "attach", "claude", "--service", "my-claude"]).unwrap();
        match claude.command {
            Some(Commands::Attach {
                target:
                    Some(AttachTarget::Claude {
                        service: Some(s), ..
                    }),
                ..
            }) => assert_eq!(s, "my-claude"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn clap_accepts_detach_targets() {
        for (args, expected) in [
            (vec!["mizpah", "detach", "shell"], DetachTarget::Shell),
            (vec!["mizpah", "detach", "cursor"], DetachTarget::Cursor),
            (vec!["mizpah", "detach", "claude"], DetachTarget::Claude),
            (vec!["mizpah", "detach", "all"], DetachTarget::All),
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            match cli.command {
                Some(Commands::Detach { target }) => assert_eq!(target, expected),
                other => panic!("unexpected: {other:?}"),
            }
        }
    }

    #[test]
    fn clap_accepts_hidden_shell_commands() {
        let init = Cli::try_parse_from(["mizpah", "__shell-init", "zsh"]).unwrap();
        match init.command {
            Some(Commands::ShellInit { shell }) => assert_eq!(shell, "zsh"),
            other => panic!("unexpected: {other:?}"),
        }

        let fwd =
            Cli::try_parse_from(["mizpah", "__shell-forward", "--tty-service", "ttys001"]).unwrap();
        match fwd.command {
            Some(Commands::ShellForward { tty_service }) => assert_eq!(tty_service, "ttys001"),
            other => panic!("unexpected: {other:?}"),
        }

        let hook = Cli::try_parse_from(["mizpah", "__hook-forward", "--source", "cursor"]).unwrap();
        match hook.command {
            Some(Commands::HookForward { source }) => assert_eq!(source, "cursor"),
            other => panic!("unexpected: {other:?}"),
        }

        let resume = Cli::try_parse_from([
            "mizpah",
            "update-resume",
            "--wait-pid",
            "12345",
            "--host",
            "127.0.0.1",
            "--port",
            "3149",
            "--project",
            "/tmp/proj",
            "--max-bytes",
            "1048576",
        ])
        .unwrap();
        match resume.command {
            Some(Commands::UpdateResume {
                wait_pid: 12345,
                hub: HubArgs { port: 3149, .. },
                max_bytes: 1048576,
                project,
                ..
            }) => assert_eq!(project, PathBuf::from("/tmp/proj")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn clap_rejects_invalid_shell_init_arity() {
        assert!(Cli::try_parse_from(["mizpah", "__shell-init"]).is_err());
    }
}
