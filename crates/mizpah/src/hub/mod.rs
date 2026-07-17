//! Hub lifecycle: defaults, health probe, spawn, start/stop/restart, PID file.

mod lifecycle;
mod pid;

pub use lifecycle::{
    ensure_hub, hub_url, probe_hub, run_hub_restart, run_hub_start, run_hub_stop,
    spawn_detached_hub_with_options,
};
pub use pid::write_hub_pid;

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 3149;
