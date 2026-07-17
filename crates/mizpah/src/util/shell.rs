//! Shell quoting helpers.

use std::path::Path;

/// Always wrap in single quotes; escape embedded `'` as `'\''`.
pub fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Quote a path for shell only when it contains characters that need quoting.
pub fn shell_quote_path(path: &Path) -> String {
    let s = path.display().to_string();
    if s.is_empty() {
        return "''".to_string();
    }
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+@%=,:".contains(c))
    {
        return s;
    }
    shell_single_quote(&s)
}
