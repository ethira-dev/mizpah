//! Clap CLI definition and command dispatch.

use crate::agent_hooks;
use crate::browser_attach;
use crate::file_ingest;
use crate::hub;
use crate::mcp;
use crate::run_cmd::{self, RunOpts};
use crate::script;
use crate::service::resolve_service;
use crate::setup::{self, SetupOpts};
use crate::shell_attach;
use crate::shell_forward;
use crate::store::{DEFAULT_MAX_BYTES, DEFAULT_TTL_HOURS};
use crate::tui;
use crate::update;
use crate::{init_tracing_stderr, run_pipe_mode};
use clap::{Args, Parser, Subcommand};
use std::path::{Path, PathBuf};
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

#[derive(Debug, Parser, Clone)]
#[command(
    about = "JSON log viewer — pipe logs and inspect them in a web UI",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Service name for this ingest stream (default: MIZPAH_SERVICE / OTEL_SERVICE_NAME / project manifests / git / dir)
    #[arg(short, long, env = "MIZPAH_SERVICE", global = false)]
    pub service: Option<String>,

    #[command(flatten)]
    pub hub: HubArgs,

    /// Max in-memory log bytes (hub only)
    #[arg(long, default_value_t = DEFAULT_MAX_BYTES)]
    pub max_bytes: u64,

    /// Drop logs older than this many hours (hub only; 0 disables)
    #[arg(long, default_value_t = DEFAULT_TTL_HOURS)]
    pub ttl_hours: u64,

    /// Do not open the browser when starting as hub
    #[arg(long, default_value_t = false)]
    pub no_open: bool,

    /// Project directory for "Check with Claude/Cursor" agent sessions (hub only)
    #[arg(long, env = "MIZPAH_PROJECT")]
    pub project: Option<PathBuf>,

    /// Allow binding the hub on a non-loopback address (prefer `[auth]` OIDC when exposing).
    /// Without this flag, only 127.0.0.1 / ::1 / localhost are accepted.
    #[arg(long, default_value_t = false)]
    pub allow_remote: bool,
}

#[derive(Debug, Subcommand, Clone)]
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

        /// Allow binding on a non-loopback address (prefer `[auth]` OIDC when exposing)
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
    /// Ingest local (or SSH) log files into a running hub
    Ingest {
        /// File paths, globs, or `user@host:path` remotes
        paths: Vec<String>,

        /// Service name for ingested lines (default: inferred project name)
        #[arg(short, long, env = "MIZPAH_SERVICE")]
        service: Option<String>,

        /// Follow files for new data (local only)
        #[arg(long, short = 'f', default_value_t = false)]
        follow: bool,

        #[command(flatten)]
        hub: HubArgs,
    },
    /// Alias for `ingest` (file paths); does not open the browser
    Files {
        /// File paths, globs, or `user@host:path` remotes
        paths: Vec<String>,

        #[arg(short, long)]
        service: Option<String>,

        #[arg(long, short = 'f', default_value_t = false)]
        follow: bool,

        #[command(flatten)]
        hub: HubArgs,
    },
    /// Query the hub with a CEL filter (prints JSON)
    Query {
        /// CEL filter expression
        #[arg(default_value = "")]
        cel: String,

        /// Group-by field paths (enables aggregate mode)
        #[arg(long = "group-by", value_delimiter = ',')]
        group_by: Vec<String>,

        #[arg(short, long, default_value_t = 50)]
        limit: usize,

        /// Output format: json (default)
        #[arg(long, default_value = "json")]
        format: String,

        /// Optional service filter
        #[arg(short, long)]
        service: Option<String>,

        #[command(flatten)]
        hub: HubArgs,
    },
    /// Run SQL against a snapshot of the hub buffer
    Sql {
        /// SELECT statement
        statement: String,

        #[command(flatten)]
        hub: HubArgs,
    },
    /// Run a line-oriented script against the hub
    Script {
        /// Script file path
        path: PathBuf,

        #[command(flatten)]
        hub: HubArgs,
    },
    /// Terminal UI against a running hub
    Tui {
        #[command(flatten)]
        hub: HubArgs,
    },
    /// One-shot agent readiness: ensure hub, install MCP configs, print next steps
    Setup {
        #[command(flatten)]
        hub: HubArgs,

        /// Project directory for the hub (defaults to cwd)
        #[arg(long, env = "MIZPAH_PROJECT")]
        project: Option<PathBuf>,

        /// Also run `npx skills add ethira-dev/mizpah`
        #[arg(long, default_value_t = false)]
        with_skill: bool,

        /// Skip writing MCP client configs
        #[arg(long, default_value_t = false)]
        skip_mcp_install: bool,
    },
    /// Read-only readiness checks (hub, MCP configs, PATH)
    Doctor {
        #[command(flatten)]
        hub: HubArgs,
    },
    /// Summarize what broke in the last N minutes
    Why {
        /// Lookback window in minutes
        #[arg(long, default_value_t = 15)]
        minutes: u64,

        #[command(flatten)]
        hub: HubArgs,
    },
    /// Ensure hub, run a command, and stream its output into Mizpah
    Run {
        /// Service name (default: MIZPAH_SERVICE / OTEL_SERVICE_NAME / project manifests / git / dir)
        #[arg(short, long, env = "MIZPAH_SERVICE")]
        service: Option<String>,

        #[command(flatten)]
        hub: HubArgs,

        /// Do not open the browser
        #[arg(long, default_value_t = false)]
        no_open: bool,

        /// Command and args (use `--` before the command)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
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

        /// Drop logs older than this many hours (0 disables)
        #[arg(long, default_value_t = DEFAULT_TTL_HOURS)]
        ttl_hours: u64,
    },
}

#[derive(Debug, Subcommand, Clone)]
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

#[derive(Debug, Subcommand, Clone)]
pub enum McpAction {
    /// Register Mizpah in Cursor, Claude Desktop, Claude Code, and Codex configs
    Install,
    /// Remove Mizpah MCP entries from those configs
    Uninstall,
}

#[derive(Debug, Subcommand, Clone)]
pub enum HubAction {
    /// Start a detached hub if one is not already healthy
    Start,
    /// Stop the hub tracked by the PID file for this port
    Stop,
    /// Stop then start the hub (clears the in-memory buffer)
    Restart,
}

#[derive(Debug, Subcommand, Clone)]
pub enum BrowserAction {
    /// Attach to Chrome/Edge DevTools and forward console + network into the hub
    Attach {
        #[command(flatten)]
        args: BrowserAttachArgs,
    },
}

/// Default service name for file ingest when `--service` is omitted.
pub(crate) fn default_ingest_service() -> String {
    resolve_service(None)
}

/// Apply config.toml defaults when CLI still has clap defaults.
pub(crate) fn apply_config_defaults(cli: &mut Cli) {
    let host_default = cli.hub.host == "127.0.0.1";
    let port_default = cli.hub.port == hub::DEFAULT_PORT;
    let max_default = cli.max_bytes == DEFAULT_MAX_BYTES;
    let ttl_default = cli.ttl_hours == DEFAULT_TTL_HOURS;
    let (host, port, max_bytes, ttl_hours) = crate::config::apply_hub_defaults(
        cli.hub.host.clone(),
        cli.hub.port,
        cli.max_bytes,
        cli.ttl_hours,
        host_default,
        port_default,
        max_default,
        ttl_default,
    );
    cli.hub.host = host;
    cli.hub.port = port;
    cli.max_bytes = max_bytes;
    cli.ttl_hours = ttl_hours;
    let _ = crate::config::ensure_default_config_file();
}

fn hub_http_base(host: &str, port: u16) -> String {
    format!("http://{host}:{port}")
}

/// Injectable side effects for CLI dispatch (real impl in production, overrides in tests).
#[derive(Default, Clone)]
pub struct CliDeps {
    skip_tracing_init: bool,
    bind_check: Option<Result<(), String>>,
    mcp_stdio: Option<Result<(), String>>,
    mcp_install: Option<i32>,
    mcp_uninstall: Option<i32>,
    shell_attach: Option<Result<(), String>>,
    browser_attach: Option<Result<(), String>>,
    attach_cursor: Option<Result<(), String>>,
    attach_claude: Option<Result<(), String>>,
    detach_shell: Option<Result<(), String>>,
    detach_cursor: Option<Result<(), String>>,
    detach_claude: Option<Result<(), String>>,
    detach_all: Option<Result<(), String>>,
    hub_start: Option<Result<(), String>>,
    hub_stop: Option<Result<(), String>>,
    hub_restart: Option<Result<(), String>>,
    resolve_open: Option<Result<(String, u16), String>>,
    run_open: Option<Result<(), String>>,
    file_ingest: Option<Result<(), String>>,
    query: Option<Result<String, String>>,
    sql: Option<Result<String, String>>,
    run_script: Option<Result<(), String>>,
    run_tui: Option<Result<(), String>>,
    shell_init: Option<Result<(), String>>,
    shell_forward: Option<Result<(), String>>,
    hook_forward: Option<Result<(), String>>,
    update_resume: Option<Result<(), String>>,
    run_pipe: Option<i32>,
    setup: Option<i32>,
    doctor: Option<i32>,
    why: Option<Result<String, String>>,
    run_cmd: Option<i32>,
}

impl CliDeps {
    pub fn real() -> Self {
        Self::default()
    }

    fn maybe_init_tracing(&self) {
        if !self.skip_tracing_init {
            init_tracing_stderr();
        }
    }

    async fn mcp_stdio(&self, host: &str, port: u16) -> Result<(), String> {
        if let Some(r) = &self.mcp_stdio {
            return r.clone();
        }
        let base_url = mcp::hub_base_url(host, port);
        mcp::run_stdio(base_url)
            .await
            .map_err(|e| e.to_string())
    }

    fn mcp_install(&self) -> i32 {
        self.mcp_install.unwrap_or_else(mcp::run_install)
    }

    fn mcp_uninstall(&self) -> i32 {
        self.mcp_uninstall.unwrap_or_else(mcp::run_uninstall)
    }

    async fn shell_attach(
        &self,
        service: Option<String>,
        host: String,
        port: u16,
    ) -> Result<(), String> {
        if let Some(r) = &self.shell_attach {
            return r.clone();
        }
        shell_attach::run_attach(service, host, port).await
    }

    async fn browser_attach(&self, opts: browser_attach::BrowserAttachOpts) -> Result<(), String> {
        if let Some(r) = &self.browser_attach {
            return r.clone();
        }
        browser_attach::run_browser_attach(opts).await
    }

    async fn attach_cursor(
        &self,
        service: Option<String>,
        host: String,
        port: u16,
    ) -> Result<(), String> {
        if let Some(r) = &self.attach_cursor {
            return r.clone();
        }
        agent_hooks::run_attach_cursor(service, host, port).await
    }

    async fn attach_claude(
        &self,
        service: Option<String>,
        host: String,
        port: u16,
    ) -> Result<(), String> {
        if let Some(r) = &self.attach_claude {
            return r.clone();
        }
        agent_hooks::run_attach_claude(service, host, port).await
    }

    fn detach_shell(&self) -> Result<(), String> {
        self.detach_shell
            .clone()
            .unwrap_or_else(shell_attach::run_detach)
    }

    fn detach_cursor(&self) -> Result<(), String> {
        self.detach_cursor
            .clone()
            .unwrap_or_else(agent_hooks::run_detach_cursor)
    }

    fn detach_claude(&self) -> Result<(), String> {
        self.detach_claude
            .clone()
            .unwrap_or_else(agent_hooks::run_detach_claude)
    }

    fn detach_all(&self) -> Result<(), String> {
        self.detach_all
            .clone()
            .unwrap_or_else(agent_hooks::run_detach_all)
    }

    async fn hub_start(
        &self,
        host: String,
        port: u16,
        project: Option<PathBuf>,
        allow_remote: bool,
    ) -> Result<(), String> {
        if let Some(r) = &self.hub_start {
            return r.clone();
        }
        hub::run_hub_start(host, port, project, allow_remote)
            .await
            .map_err(|e| e.to_string())
    }

    async fn hub_stop(&self, host: String, port: u16) -> Result<(), String> {
        if let Some(r) = &self.hub_stop {
            return r.clone();
        }
        hub::run_hub_stop(host, port)
            .await
            .map_err(|e| e.to_string())
    }

    async fn hub_restart(
        &self,
        host: String,
        port: u16,
        project: Option<PathBuf>,
        allow_remote: bool,
    ) -> Result<(), String> {
        if let Some(r) = &self.hub_restart {
            return r.clone();
        }
        hub::run_hub_restart(host, port, project, allow_remote)
            .await
            .map_err(|e| e.to_string())
    }

    fn bind_check(&self, host: &str, allow_remote: bool) -> Result<(), String> {
        if let Some(r) = &self.bind_check {
            return r.clone();
        }
        let auth_enabled = crate::config::MizpahConfig::load().auth.enabled;
        crate::check_bind_allowed(host, allow_remote, auth_enabled)
    }

    fn resolve_open_target(
        &self,
        host: Option<String>,
        port: Option<u16>,
    ) -> Result<(String, u16), String> {
        if let Some(r) = &self.resolve_open {
            return r.clone();
        }
        shell_attach::resolve_open_target(host, port)
    }

    async fn run_open(&self, host: String, port: u16) -> Result<(), String> {
        if let Some(r) = &self.run_open {
            return r.clone();
        }
        shell_attach::run_open(host, port).await
    }

    async fn file_ingest(
        &self,
        paths: Vec<String>,
        service: String,
        follow: bool,
        host: &str,
        port: u16,
    ) -> Result<(), String> {
        if let Some(r) = &self.file_ingest {
            return r.clone();
        }
        file_ingest::run_ingest(paths, service, follow, host, port)
            .await
            .map_err(|e| e.to_string())
    }

    async fn query_logs(
        &self,
        base: &str,
        cel: &str,
        group_by: &[String],
        limit: usize,
        service: Option<&str>,
    ) -> Result<String, String> {
        if let Some(r) = &self.query {
            return r.clone();
        }
        let client = mcp::HubClient::new(base);
        if group_by.is_empty() {
            client
                .search_logs(service, Some(cel), Some(limit), None)
                .await
                .map(|r| serde_json::to_string_pretty(&r).unwrap_or_default())
                .map_err(|e| e.to_string())
        } else {
            client
                .aggregate_logs(service, Some(cel), group_by, Some(limit))
                .await
                .map(|r| serde_json::to_string_pretty(&r).unwrap_or_default())
                .map_err(|e| e.to_string())
        }
    }

    async fn query_sql(&self, base: &str, statement: &str) -> Result<String, String> {
        if let Some(r) = &self.sql {
            return r.clone();
        }
        let client = mcp::HubClient::new(base);
        client
            .query_sql(statement, Some(200))
            .await
            .map(|v| serde_json::to_string_pretty(&v).unwrap_or_default())
            .map_err(|e| e.to_string())
    }

    async fn run_script(&self, path: &Path, base: &str) -> Result<(), String> {
        if let Some(r) = &self.run_script {
            return r.clone();
        }
        script::run_script(path, base)
            .await
            .map_err(|e| e.to_string())
    }

    async fn run_tui(&self, host: &str, port: u16) -> Result<(), String> {
        if let Some(r) = &self.run_tui {
            return r.clone();
        }
        let _ = crate::keymap::Keymap::ensure_default_file();
        let _ = crate::keymap::themes::ensure_default_themes();
        tui::run_tui(host, port).await.map_err(|e| e.to_string())
    }

    fn shell_init(&self, shell: &str) -> Result<(), String> {
        if let Some(r) = &self.shell_init {
            return r.clone();
        }
        shell_attach::run_shell_init(shell)
    }

    async fn shell_forward(&self, tty_service: String) -> Result<(), String> {
        if let Some(r) = &self.shell_forward {
            return r.clone();
        }
        shell_forward::run_shell_forward(tty_service).await
    }

    async fn hook_forward(&self, source: agent_hooks::HookSource) -> Result<(), String> {
        if let Some(r) = &self.hook_forward {
            return r.clone();
        }
        agent_hooks::run_hook_forward(source).await;
        Ok(())
    }

    async fn update_resume(
        &self,
        wait_pid: u32,
        host: String,
        port: u16,
        project: PathBuf,
        max_bytes: u64,
        ttl_hours: u64,
    ) -> Result<(), String> {
        if let Some(r) = &self.update_resume {
            return r.clone();
        }
        update::run_update_resume(wait_pid, host, port, project, max_bytes, ttl_hours).await
    }

    async fn run_pipe_mode(&self, cli: Cli) -> i32 {
        if let Some(code) = self.run_pipe {
            return code;
        }
        match run_pipe_mode(cli).await {
            Ok(()) => 0,
            Err(code) => code,
        }
    }

    async fn setup(&self, opts: SetupOpts) -> i32 {
        if let Some(code) = self.setup {
            return code;
        }
        setup::run_setup(opts).await
    }

    async fn doctor(&self, host: &str, port: u16) -> i32 {
        if let Some(code) = self.doctor {
            return code;
        }
        setup::run_doctor(host, port).await
    }

    async fn why(&self, host: &str, port: u16, minutes: u64) -> Result<String, String> {
        if let Some(r) = &self.why {
            return r.clone();
        }
        let client = mcp::HubClient::new(hub_http_base(host, port));
        client
            .get_incident(minutes)
            .await
            .map(|v| serde_json::to_string_pretty(&v).unwrap_or_default())
            .map_err(|e| e.to_string())
    }

    async fn run_cmd(&self, opts: RunOpts) -> i32 {
        if let Some(code) = self.run_cmd {
            return code;
        }
        run_cmd::run_command(opts).await
    }
}

/// Dispatch parsed CLI. Returns process exit code (0 = success).
pub async fn run_parsed(cli: Cli, deps: &CliDeps) -> i32 {
    match cli.command {
        Some(Commands::Mcp { action, hub }) => match action {
            None => {
                deps.maybe_init_tracing();
                match deps.mcp_stdio(&hub.host, hub.port).await {
                    Ok(()) => 0,
                    Err(err) => {
                        error!(error = %err, "MCP server failed");
                        1
                    }
                }
            }
            Some(McpAction::Install) => deps.mcp_install(),
            Some(McpAction::Uninstall) => deps.mcp_uninstall(),
        },
        Some(Commands::Attach {
            target,
            service,
            hub,
        }) => {
            deps.maybe_init_tracing();
            let result = match target {
                None => deps.shell_attach(service, hub.host, hub.port).await,
                Some(AttachTarget::Shell { service, hub }) => {
                    deps.shell_attach(service, hub.host, hub.port).await
                }
                Some(AttachTarget::Browser { args }) => deps.browser_attach(args.into_opts()).await,
                Some(AttachTarget::Cursor { service, hub }) => {
                    deps.attach_cursor(service, hub.host, hub.port).await
                }
                Some(AttachTarget::Claude { service, hub }) => {
                    deps.attach_claude(service, hub.host, hub.port).await
                }
            };
            match result {
                Ok(()) => 0,
                Err(err) => {
                    eprintln!("error: {err}");
                    1
                }
            }
        }
        Some(Commands::Detach { target }) => {
            deps.maybe_init_tracing();
            let result = match target {
                DetachTarget::Shell => deps.detach_shell(),
                DetachTarget::Cursor => deps.detach_cursor(),
                DetachTarget::Claude => deps.detach_claude(),
                DetachTarget::All => deps.detach_all(),
            };
            match result {
                Ok(()) => 0,
                Err(err) => {
                    eprintln!("error: {err}");
                    1
                }
            }
        }
        Some(Commands::Browser { action }) => {
            deps.maybe_init_tracing();
            match action {
                BrowserAction::Attach { args } => match deps.browser_attach(args.into_opts()).await
                {
                    Ok(()) => 0,
                    Err(err) => {
                        eprintln!("error: {err}");
                        1
                    }
                },
            }
        }
        Some(Commands::Hub {
            action,
            hub,
            project,
            allow_remote,
        }) => {
            deps.maybe_init_tracing();
            if matches!(action, HubAction::Start | HubAction::Restart) {
                if let Err(msg) = deps.bind_check(&hub.host, allow_remote) {
                    eprintln!("{msg}");
                    return 2;
                }
            }
            let result = match action {
                HubAction::Start => {
                    deps.hub_start(hub.host, hub.port, project, allow_remote)
                        .await
                }
                HubAction::Stop => deps.hub_stop(hub.host, hub.port).await,
                HubAction::Restart => {
                    deps.hub_restart(hub.host, hub.port, project, allow_remote)
                        .await
                }
            };
            match result {
                Ok(()) => 0,
                Err(err) => {
                    eprintln!("error: {err}");
                    1
                }
            }
        }
        Some(Commands::Open { host, port }) => {
            deps.maybe_init_tracing();
            let (host, port) = match deps.resolve_open_target(host, port) {
                Ok(t) => t,
                Err(err) => {
                    eprintln!("error: {err}");
                    return 1;
                }
            };
            match deps.run_open(host, port).await {
                Ok(()) => 0,
                Err(err) => {
                    eprintln!("error: {err}");
                    1
                }
            }
        }
        Some(Commands::Ingest {
            paths,
            service,
            follow,
            hub,
        })
        | Some(Commands::Files {
            paths,
            service,
            follow,
            hub,
        }) => {
            deps.maybe_init_tracing();
            if paths.is_empty() {
                eprintln!("error: provide at least one file path");
                return 2;
            }
            let service = service.unwrap_or_else(default_ingest_service);
            match deps
                .file_ingest(paths, service, follow, &hub.host, hub.port)
                .await
            {
                Ok(()) => 0,
                Err(err) => {
                    eprintln!("error: {err}");
                    1
                }
            }
        }
        Some(Commands::Query {
            cel,
            group_by,
            limit,
            format,
            service,
            hub,
        }) => {
            deps.maybe_init_tracing();
            let base = hub_http_base(&hub.host, hub.port);
            match deps
                .query_logs(&base, &cel, &group_by, limit, service.as_deref())
                .await
            {
                Ok(text) => {
                    if format != "json" {
                        eprintln!("warning: only json format is supported; printing json");
                    }
                    println!("{text}");
                    0
                }
                Err(err) => {
                    eprintln!("error: {err}");
                    1
                }
            }
        }
        Some(Commands::Sql { statement, hub }) => {
            deps.maybe_init_tracing();
            let base = hub_http_base(&hub.host, hub.port);
            match deps.query_sql(&base, &statement).await {
                Ok(text) => {
                    println!("{text}");
                    0
                }
                Err(err) => {
                    eprintln!("error: {err}");
                    1
                }
            }
        }
        Some(Commands::Script { path, hub }) => {
            deps.maybe_init_tracing();
            let base = hub_http_base(&hub.host, hub.port);
            match deps.run_script(&path, &base).await {
                Ok(()) => 0,
                Err(err) => {
                    eprintln!("error: {err}");
                    1
                }
            }
        }
        Some(Commands::Tui { hub }) => {
            deps.maybe_init_tracing();
            match deps.run_tui(&hub.host, hub.port).await {
                Ok(()) => 0,
                Err(err) => {
                    eprintln!("error: {err}");
                    1
                }
            }
        }
        Some(Commands::Setup {
            hub,
            project,
            with_skill,
            skip_mcp_install,
        }) => {
            deps.maybe_init_tracing();
            deps.setup(SetupOpts {
                host: hub.host,
                port: hub.port,
                project,
                with_skill,
                skip_mcp_install,
            })
            .await
        }
        Some(Commands::Doctor { hub }) => {
            deps.maybe_init_tracing();
            deps.doctor(&hub.host, hub.port).await
        }
        Some(Commands::Why { minutes, hub }) => {
            deps.maybe_init_tracing();
            match deps.why(&hub.host, hub.port, minutes).await {
                Ok(text) => {
                    println!("{text}");
                    0
                }
                Err(err) => {
                    eprintln!("error: {err}");
                    1
                }
            }
        }
        Some(Commands::Run {
            service,
            hub,
            no_open,
            args,
        }) => {
            deps.maybe_init_tracing();
            let service = resolve_service(service.as_deref());
            deps.run_cmd(RunOpts {
                service,
                host: hub.host,
                port: hub.port,
                no_open,
                args,
            })
            .await
        }
        Some(Commands::ShellInit { shell }) => match deps.shell_init(&shell) {
            Ok(()) => 0,
            Err(err) => {
                eprintln!("error: {err}");
                1
            }
        },
        Some(Commands::ShellForward { tty_service }) => {
            deps.maybe_init_tracing();
            match deps.shell_forward(tty_service).await {
                Ok(()) => 0,
                Err(err) => {
                    eprintln!("error: {err}");
                    1
                }
            }
        }
        Some(Commands::HookForward { source }) => {
            deps.maybe_init_tracing();
            let Some(src) = agent_hooks::HookSource::parse(&source) else {
                return 0;
            };
            let _ = deps.hook_forward(src).await;
            0
        }
        Some(Commands::UpdateResume {
            wait_pid,
            hub,
            project,
            max_bytes,
            ttl_hours,
        }) => {
            deps.maybe_init_tracing();
            match deps
                .update_resume(wait_pid, hub.host, hub.port, project, max_bytes, ttl_hours)
                .await
            {
                Ok(()) => 0,
                Err(err) => {
                    eprintln!("error: {err}");
                    1
                }
            }
        }
        None => {
            deps.maybe_init_tracing();
            deps.run_pipe_mode(cli).await
        }
    }
}

pub async fn run() {
    let code = run_with_cli(Cli::parse(), &CliDeps::real()).await;
    if code != 0 {
        std::process::exit(code);
    }
}

/// Parse-free entry for tests and `run()`.
pub(crate) async fn run_with_cli(mut cli: Cli, deps: &CliDeps) -> i32 {
    crate::util::ensure_rustls_crypto_provider();
    apply_config_defaults(&mut cli);
    run_parsed(cli, deps).await
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
        assert!(help.contains("ingest"));
        assert!(help.contains("query"));
        assert!(help.contains("sql"));
        assert!(help.contains("tui"));
    }

    #[test]
    fn clap_accepts_ingest_and_query() {
        let ingest = Cli::try_parse_from([
            "mizpah",
            "ingest",
            "/tmp/a.log",
            "--service",
            "api",
            "--follow",
        ])
        .unwrap();
        assert!(matches!(
            ingest.command,
            Some(Commands::Ingest { follow: true, .. })
        ));

        let q = Cli::try_parse_from([
            "mizpah",
            "query",
            r#"level == "error""#,
            "--group-by",
            "level",
            "--limit",
            "10",
        ])
        .unwrap();
        match q.command {
            Some(Commands::Query {
                group_by,
                limit: 10,
                ..
            }) => assert_eq!(group_by, vec!["level".to_string()]),
            other => panic!("unexpected: {other:?}"),
        }
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
            "--ttl-hours",
            "12",
        ])
        .unwrap();
        match resume.command {
            Some(Commands::UpdateResume {
                wait_pid: 12345,
                hub: HubArgs { port: 3149, .. },
                max_bytes: 1048576,
                ttl_hours: 12,
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

    fn test_deps() -> CliDeps {
        CliDeps {
            skip_tracing_init: true,
            ..CliDeps::real()
        }
    }

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("parse")
    }

    #[test]
    fn browser_attach_args_into_opts() {
        let args = BrowserAttachArgs {
            service: Some("web".into()),
            hub: HubArgs {
                host: "127.0.0.1".into(),
                port: 3149,
            },
            cdp_port: 9223,
            cdp_url: Some("ws://x".into()),
            launch: true,
            all_network: true,
        };
        let opts = args.into_opts();
        assert_eq!(opts.service.as_deref(), Some("web"));
        assert_eq!(opts.host, "127.0.0.1");
        assert_eq!(opts.port, 3149);
        assert_eq!(opts.cdp_port, 9223);
        assert_eq!(opts.cdp_url.as_deref(), Some("ws://x"));
        assert!(opts.launch);
        assert!(opts.all_network);
    }

    #[test]
    fn default_ingest_service_non_empty() {
        let s = default_ingest_service();
        assert!(!s.is_empty());
        assert!(!s.contains('/'));
    }

    #[test]
    fn clap_parses_setup_doctor_why_run() {
        let setup = parse(&[
            "mizpah",
            "setup",
            "--with-skill",
            "--skip-mcp-install",
            "--port",
            "4000",
        ]);
        match setup.command {
            Some(Commands::Setup {
                hub,
                with_skill,
                skip_mcp_install,
                ..
            }) => {
                assert!(with_skill);
                assert!(skip_mcp_install);
                assert_eq!(hub.port, 4000);
            }
            other => panic!("expected Setup, got {other:?}"),
        }

        let doctor = parse(&["mizpah", "doctor", "--host", "127.0.0.1"]);
        assert!(matches!(doctor.command, Some(Commands::Doctor { .. })));

        let why = parse(&["mizpah", "why", "--minutes", "30"]);
        match why.command {
            Some(Commands::Why { minutes, .. }) => assert_eq!(minutes, 30),
            other => panic!("expected Why, got {other:?}"),
        }

        let run = parse(&[
            "mizpah",
            "run",
            "--service",
            "api",
            "--no-open",
            "--",
            "npm",
            "test",
            "--watch",
        ]);
        match run.command {
            Some(Commands::Run {
                service,
                no_open,
                args,
                ..
            }) => {
                assert_eq!(service.as_deref(), Some("api"));
                assert!(no_open);
                assert_eq!(args, vec!["npm", "test", "--watch"]);
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn apply_config_defaults_smoke() {
        let _guard = crate::test_support::env_lock();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("MIZPAH_CONFIG_DIR", dir.path());
        let mut cli = parse(&["mizpah", "--no-open"]);
        apply_config_defaults(&mut cli);
        assert_eq!(cli.hub.host, "127.0.0.1");
    }

    #[tokio::test]
    async fn run_parsed_mcp_stdio_ok_and_err() {
        let deps = CliDeps {
            skip_tracing_init: true,
            mcp_stdio: Some(Ok(())),
            ..test_deps()
        };
        let cli = parse(&["mizpah", "mcp"]);
        assert_eq!(run_parsed(cli, &deps).await, 0);

        let deps = CliDeps {
            mcp_stdio: Some(Err("boom".into())),
            ..test_deps()
        };
        assert_eq!(run_parsed(parse(&["mizpah", "mcp"]), &deps).await, 1);
    }

    #[tokio::test]
    async fn run_parsed_mcp_install_uninstall() {
        let deps = CliDeps {
            mcp_install: Some(42),
            ..test_deps()
        };
        assert_eq!(
            run_parsed(parse(&["mizpah", "mcp", "install"]), &deps).await,
            42
        );

        let deps = CliDeps {
            mcp_uninstall: Some(7),
            ..test_deps()
        };
        assert_eq!(
            run_parsed(parse(&["mizpah", "mcp", "uninstall"]), &deps).await,
            7
        );
    }

    #[tokio::test]
    async fn run_parsed_attach_all_targets() {
        let ok = CliDeps {
            shell_attach: Some(Ok(())),
            browser_attach: Some(Ok(())),
            attach_cursor: Some(Ok(())),
            attach_claude: Some(Ok(())),
            ..test_deps()
        };
        assert_eq!(run_parsed(parse(&["mizpah", "attach"]), &ok).await, 0);
        assert_eq!(
            run_parsed(parse(&["mizpah", "attach", "shell"]), &ok).await,
            0
        );
        assert_eq!(
            run_parsed(parse(&["mizpah", "attach", "browser"]), &ok).await,
            0
        );
        assert_eq!(
            run_parsed(parse(&["mizpah", "attach", "cursor"]), &ok).await,
            0
        );
        assert_eq!(
            run_parsed(parse(&["mizpah", "attach", "claude"]), &ok).await,
            0
        );

        let err = CliDeps {
            shell_attach: Some(Err("attach fail".into())),
            ..test_deps()
        };
        assert_eq!(run_parsed(parse(&["mizpah", "attach"]), &err).await, 1);
    }

    #[tokio::test]
    async fn run_parsed_detach_all_targets() {
        let ok = CliDeps {
            detach_shell: Some(Ok(())),
            detach_cursor: Some(Ok(())),
            detach_claude: Some(Ok(())),
            detach_all: Some(Ok(())),
            ..test_deps()
        };
        for args in [
            vec!["mizpah", "detach"],
            vec!["mizpah", "detach", "shell"],
            vec!["mizpah", "detach", "cursor"],
            vec!["mizpah", "detach", "claude"],
            vec!["mizpah", "detach", "all"],
        ] {
            assert_eq!(run_parsed(parse(&args), &ok).await, 0, "{args:?}");
        }

        let err = CliDeps {
            detach_all: Some(Err("detach fail".into())),
            ..test_deps()
        };
        assert_eq!(
            run_parsed(parse(&["mizpah", "detach", "all"]), &err).await,
            1
        );
    }

    #[tokio::test]
    async fn run_parsed_browser_attach() {
        let ok = CliDeps {
            browser_attach: Some(Ok(())),
            ..test_deps()
        };
        assert_eq!(
            run_parsed(parse(&["mizpah", "browser", "attach"]), &ok).await,
            0
        );

        let err = CliDeps {
            browser_attach: Some(Err("browser fail".into())),
            ..test_deps()
        };
        assert_eq!(
            run_parsed(parse(&["mizpah", "browser", "attach"]), &err).await,
            1
        );
    }

    #[tokio::test]
    async fn run_parsed_hub_actions() {
        let ok = CliDeps {
            hub_start: Some(Ok(())),
            hub_stop: Some(Ok(())),
            hub_restart: Some(Ok(())),
            ..test_deps()
        };
        assert_eq!(run_parsed(parse(&["mizpah", "hub", "start"]), &ok).await, 0);
        assert_eq!(run_parsed(parse(&["mizpah", "hub", "stop"]), &ok).await, 0);
        assert_eq!(
            run_parsed(parse(&["mizpah", "hub", "restart"]), &ok).await,
            0
        );

        let err = CliDeps {
            hub_stop: Some(Err("stop fail".into())),
            ..test_deps()
        };
        assert_eq!(run_parsed(parse(&["mizpah", "hub", "stop"]), &err).await, 1);

        let bind_err = CliDeps {
            bind_check: Some(Err("bind denied".into())),
            hub_stop: Some(Ok(())),
            ..test_deps()
        };
        assert_eq!(
            run_parsed(parse(&["mizpah", "hub", "start"]), &bind_err).await,
            2
        );
        assert_eq!(
            run_parsed(parse(&["mizpah", "hub", "restart"]), &bind_err).await,
            2
        );
        // Stop does not run bind check.
        assert_eq!(
            run_parsed(parse(&["mizpah", "hub", "stop"]), &bind_err).await,
            0
        );
    }

    #[tokio::test]
    async fn run_parsed_open() {
        let ok = CliDeps {
            resolve_open: Some(Ok(("127.0.0.1".into(), 3149))),
            run_open: Some(Ok(())),
            ..test_deps()
        };
        assert_eq!(run_parsed(parse(&["mizpah", "open"]), &ok).await, 0);

        let resolve_err = CliDeps {
            resolve_open: Some(Err("no state".into())),
            ..test_deps()
        };
        assert_eq!(
            run_parsed(parse(&["mizpah", "open"]), &resolve_err).await,
            1
        );

        let open_err = CliDeps {
            resolve_open: Some(Ok(("127.0.0.1".into(), 3149))),
            run_open: Some(Err("hub down".into())),
            ..test_deps()
        };
        assert_eq!(run_parsed(parse(&["mizpah", "open"]), &open_err).await, 1);
    }

    #[tokio::test]
    async fn run_parsed_ingest_and_files() {
        let ok = CliDeps {
            file_ingest: Some(Ok(())),
            ..test_deps()
        };
        assert_eq!(
            run_parsed(parse(&["mizpah", "ingest", "/tmp/a.log"]), &ok).await,
            0
        );
        assert_eq!(
            run_parsed(parse(&["mizpah", "files", "/tmp/a.log"]), &ok).await,
            0
        );

        let empty = test_deps();
        assert_eq!(run_parsed(parse(&["mizpah", "ingest"]), &empty).await, 2);
        assert_eq!(run_parsed(parse(&["mizpah", "files"]), &empty).await, 2);

        let err = CliDeps {
            file_ingest: Some(Err("ingest fail".into())),
            ..test_deps()
        };
        assert_eq!(
            run_parsed(parse(&["mizpah", "ingest", "/tmp/a.log"]), &err).await,
            1
        );
    }

    #[tokio::test]
    async fn run_parsed_query_and_sql() {
        let deps = CliDeps {
            query: Some(Ok(r#"{"entries":[]}"#.into())),
            sql: Some(Ok(r#"{"columns":[]}"#.into())),
            ..test_deps()
        };
        assert_eq!(
            run_parsed(parse(&["mizpah", "query", "true"]), &deps).await,
            0
        );
        assert_eq!(
            run_parsed(
                parse(&[
                    "mizpah",
                    "query",
                    "true",
                    "--group-by",
                    "level",
                    "--format",
                    "text"
                ]),
                &deps
            )
            .await,
            0
        );
        assert_eq!(
            run_parsed(parse(&["mizpah", "sql", "SELECT 1"]), &deps).await,
            0
        );

        let err = CliDeps {
            query: Some(Err("query fail".into())),
            ..test_deps()
        };
        assert_eq!(
            run_parsed(parse(&["mizpah", "query", "true"]), &err).await,
            1
        );
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn run_parsed_query_sql_live_hub() {
        let (base, _store) = crate::test_support::spawn_test_hub().await;
        let port: u16 = base.rsplit(':').next().unwrap().parse().unwrap();
        let deps = test_deps();
        assert_eq!(
            run_parsed(
                parse(&[
                    "mizpah",
                    "query",
                    "true",
                    "--host",
                    "127.0.0.1",
                    "-p",
                    &port.to_string()
                ]),
                &deps
            )
            .await,
            0
        );
        assert_eq!(
            run_parsed(
                parse(&[
                    "mizpah",
                    "query",
                    "true",
                    "--group-by",
                    "level",
                    "--host",
                    "127.0.0.1",
                    "-p",
                    &port.to_string()
                ]),
                &deps
            )
            .await,
            0
        );
        assert_eq!(
            run_parsed(
                parse(&[
                    "mizpah",
                    "sql",
                    "SELECT 1",
                    "--host",
                    "127.0.0.1",
                    "-p",
                    &port.to_string()
                ]),
                &deps
            )
            .await,
            0
        );
    }

    #[tokio::test]
    async fn run_parsed_script_tui_and_internal() {
        let ok = CliDeps {
            run_script: Some(Ok(())),
            run_tui: Some(Ok(())),
            shell_init: Some(Ok(())),
            shell_forward: Some(Ok(())),
            hook_forward: Some(Ok(())),
            update_resume: Some(Ok(())),
            ..test_deps()
        };
        assert_eq!(
            run_parsed(parse(&["mizpah", "script", "/tmp/x.mzp"]), &ok).await,
            0
        );
        assert_eq!(run_parsed(parse(&["mizpah", "tui"]), &ok).await, 0);
        assert_eq!(
            run_parsed(parse(&["mizpah", "__shell-init", "zsh"]), &ok).await,
            0
        );
        assert_eq!(
            run_parsed(
                parse(&["mizpah", "__shell-forward", "--tty-service", "ttys001"]),
                &ok
            )
            .await,
            0
        );
        assert_eq!(
            run_parsed(
                parse(&["mizpah", "__hook-forward", "--source", "cursor"]),
                &ok
            )
            .await,
            0
        );
        assert_eq!(
            run_parsed(
                parse(&[
                    "mizpah",
                    "update-resume",
                    "--wait-pid",
                    "1",
                    "--project",
                    "/tmp/p"
                ]),
                &ok
            )
            .await,
            0
        );

        let hook_bad = test_deps();
        assert_eq!(
            run_parsed(
                parse(&["mizpah", "__hook-forward", "--source", "unknown"]),
                &hook_bad
            )
            .await,
            0
        );

        let err = CliDeps {
            run_script: Some(Err("script fail".into())),
            run_tui: Some(Err("tui fail".into())),
            shell_init: Some(Err("init fail".into())),
            shell_forward: Some(Err("fwd fail".into())),
            update_resume: Some(Err("resume fail".into())),
            ..test_deps()
        };
        assert_eq!(
            run_parsed(parse(&["mizpah", "script", "/tmp/x.mzp"]), &err).await,
            1
        );
        assert_eq!(run_parsed(parse(&["mizpah", "tui"]), &err).await, 1);
        assert_eq!(
            run_parsed(parse(&["mizpah", "__shell-init", "zsh"]), &err).await,
            1
        );
        assert_eq!(
            run_parsed(
                parse(&["mizpah", "__shell-forward", "--tty-service", "ttys001"]),
                &err
            )
            .await,
            1
        );
        assert_eq!(
            run_parsed(
                parse(&[
                    "mizpah",
                    "update-resume",
                    "--wait-pid",
                    "1",
                    "--project",
                    "/tmp/p"
                ]),
                &err
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn run_parsed_pipe_mode_delegates() {
        let deps = CliDeps {
            run_pipe: Some(0),
            ..test_deps()
        };
        assert_eq!(run_parsed(parse(&["mizpah", "--no-open"]), &deps).await, 0);

        let deps = CliDeps {
            run_pipe: Some(3),
            ..test_deps()
        };
        assert_eq!(run_parsed(parse(&["mizpah", "--no-open"]), &deps).await, 3);
    }

    // Real CliDeps hits reqwest (setsockopt keepalive) — unsupported under Miri.
    #[cfg(not(miri))]
    #[tokio::test]
    async fn run_parsed_real_deps_fast_failures() {
        let _guard = crate::test_support::env_lock();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("MIZPAH_CONFIG_DIR", dir.path());
        std::env::set_var("HOME", dir.path());

        let deps = CliDeps {
            skip_tracing_init: true,
            ..CliDeps::real()
        };

        // Hit real CliDeps delegations that fail quickly without hanging.
        for args in [
            vec!["mizpah", "detach"],
            vec!["mizpah", "detach", "shell"],
            vec!["mizpah", "detach", "cursor"],
            vec!["mizpah", "detach", "claude"],
            vec!["mizpah", "detach", "all"],
        ] {
            assert!(run_parsed(parse(&args), &deps).await <= 1, "{args:?}");
        }
        assert_eq!(
            run_parsed(parse(&["mizpah", "__shell-init", "zsh"]), &deps).await,
            0
        );
        assert_eq!(
            run_parsed(parse(&["mizpah", "__shell-init", "bash"]), &deps).await,
            0
        );
        assert!(run_parsed(parse(&["mizpah", "hub", "stop", "-p", "1"]), &deps).await <= 1);
        assert_eq!(
            run_parsed(parse(&["mizpah", "ingest", "/nonexistent/file.log"]), &deps).await,
            1
        );
        assert_eq!(
            run_parsed(parse(&["mizpah", "query", "true", "-p", "1"]), &deps).await,
            1
        );
        assert_eq!(
            run_parsed(parse(&["mizpah", "sql", "SELECT 1", "-p", "1"]), &deps).await,
            1
        );
        assert_eq!(
            run_parsed(parse(&["mizpah", "script", "/nonexistent.mzp"]), &deps).await,
            1
        );
        assert!(run_parsed(parse(&["mizpah", "open", "-p", "1"]), &deps).await <= 1);
        assert!(run_parsed(parse(&["mizpah", "attach", "-p", "59999"]), &deps).await <= 1);
        assert!(
            run_parsed(parse(&["mizpah", "attach", "cursor", "-p", "59999"]), &deps).await <= 1
        );
        assert!(
            run_parsed(parse(&["mizpah", "attach", "claude", "-p", "59999"]), &deps).await <= 1
        );
        assert!(
            run_parsed(
                parse(&["mizpah", "attach", "browser", "-p", "59999"]),
                &deps
            )
            .await
                <= 1
        );
        assert!(
            run_parsed(
                parse(&["mizpah", "browser", "attach", "-p", "59999"]),
                &deps
            )
            .await
                <= 1
        );
        assert_eq!(
            run_parsed(
                parse(&["mizpah", "__hook-forward", "--source", "cursor"]),
                &deps
            )
            .await,
            0
        );
        assert_eq!(
            run_parsed(
                parse(&["mizpah", "__hook-forward", "--source", "claude"]),
                &deps
            )
            .await,
            0
        );
        // Real installer entry points (temp HOME — no user config mutation).
        assert!(run_parsed(parse(&["mizpah", "mcp", "install"]), &deps).await <= 1);
        assert!(run_parsed(parse(&["mizpah", "mcp", "uninstall"]), &deps).await <= 1);
    }

    #[tokio::test]
    async fn run_parsed_sql_error_mock() {
        let deps = CliDeps {
            sql: Some(Err("sql fail".into())),
            ..test_deps()
        };
        assert_eq!(
            run_parsed(parse(&["mizpah", "sql", "SELECT 1"]), &deps).await,
            1
        );
    }

    #[tokio::test]
    async fn run_parsed_hub_start_restart_errors() {
        let err = CliDeps {
            hub_start: Some(Err("start fail".into())),
            hub_restart: Some(Err("restart fail".into())),
            ..test_deps()
        };
        assert_eq!(
            run_parsed(parse(&["mizpah", "hub", "start"]), &err).await,
            1
        );
        assert_eq!(
            run_parsed(parse(&["mizpah", "hub", "restart"]), &err).await,
            1
        );
    }

    #[tokio::test]
    async fn run_with_cli_applies_config_and_dispatches() {
        let _guard = crate::test_support::env_lock();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("MIZPAH_CONFIG_DIR", dir.path());
        let deps = CliDeps {
            run_pipe: Some(0),
            skip_tracing_init: true,
            ..CliDeps::real()
        };
        let cli = parse(&["mizpah", "--no-open"]);
        assert_eq!(run_with_cli(cli, &deps).await, 0);
    }
}
