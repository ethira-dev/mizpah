//! Shared filesystem, PATH, and shell-quoting helpers.

mod fs;
mod path;
mod shell;

pub use fs::{atomic_write, config_dir, home_dir};
pub use path::which;
pub use shell::{shell_quote_path, shell_single_quote};

/// Install the rustls `ring` provider once (reqwest uses `rustls-no-provider`).
pub fn ensure_rustls_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
