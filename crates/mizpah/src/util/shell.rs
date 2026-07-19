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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn shell_single_quote_escapes_apostrophe() {
        assert_eq!(shell_single_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn shell_quote_path_empty() {
        assert_eq!(shell_quote_path(Path::new("")), "''");
    }

    #[test]
    fn shell_quote_path_safe_chars_unquoted() {
        assert_eq!(shell_quote_path(Path::new("/tmp/foo-bar_1.txt")), "/tmp/foo-bar_1.txt");
    }

    #[test]
    fn shell_quote_path_spaces() {
        assert_eq!(shell_quote_path(Path::new("/tmp/my file")), "'/tmp/my file'");
    }

    #[test]
    fn shell_quote_path_apostrophe() {
        assert_eq!(
            shell_quote_path(Path::new("/tmp/bob's")),
            "'/tmp/bob'\\''s'"
        );
    }
}
