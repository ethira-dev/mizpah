//! Keymap loading with built-in defaults (Phase I).

use crate::config::MizpahConfig;
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Keymap {
    /// Next error / warn
    pub next_error: String,
    /// Previous error / warn
    pub prev_error: String,
    pub down: String,
    pub up: String,
    pub quit: String,
    /// Open / refresh related trace for selected row (TUI).
    pub show_trace: String,
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            next_error: "e".into(),
            prev_error: "E".into(),
            down: "j".into(),
            up: "k".into(),
            quit: "q".into(),
            show_trace: "t".into(),
        }
    }
}

impl Keymap {
    pub fn load() -> Self {
        match MizpahConfig::keymaps_path() {
            Ok(path) if path.exists() => match fs::read_to_string(&path) {
                Ok(text) => toml::from_str(&text).unwrap_or_default(),
                Err(_) => Self::default(),
            },
            _ => Self::default(),
        }
    }

    /// Ensure a default keymaps.toml exists.
    pub fn ensure_default_file() -> std::io::Result<()> {
        let _ = MizpahConfig::ensure_layout()?;
        let path = MizpahConfig::keymaps_path()?;
        if !path.exists() {
            let text = toml::to_string_pretty(&Self::default())
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            crate::util::atomic_write(&path, &text)?;
        }
        Ok(())
    }
}

/// Themes — CSS variable packs for the web UI / TUI accent hints.
pub mod themes {
    use crate::config::MizpahConfig;
    use serde::{Deserialize, Serialize};
    use std::fs;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(default, rename_all = "camelCase")]
    pub struct Theme {
        pub name: String,
        pub background: String,
        pub foreground: String,
        pub accent: String,
        pub muted: String,
        pub error: String,
        pub warn: String,
    }

    impl Default for Theme {
        fn default() -> Self {
            default_theme()
        }
    }

    pub fn default_theme() -> Theme {
        Theme {
            name: "default".into(),
            background: "#0f1419".into(),
            foreground: "#e7ecf3".into(),
            accent: "#3d9a6a".into(),
            muted: "#8b98a8".into(),
            error: "#e35d6a".into(),
            warn: "#d4a017".into(),
        }
    }

    pub fn slate_theme() -> Theme {
        Theme {
            name: "slate".into(),
            background: "#111827".into(),
            foreground: "#f3f4f6".into(),
            accent: "#60a5fa".into(),
            muted: "#9ca3af".into(),
            error: "#f87171".into(),
            warn: "#fbbf24".into(),
        }
    }

    pub fn ensure_default_themes() -> std::io::Result<()> {
        let _ = MizpahConfig::ensure_layout()?;
        let dir = MizpahConfig::themes_dir()?;
        for theme in [default_theme(), slate_theme()] {
            let path = dir.join(format!("{}.toml", theme.name));
            if !path.exists() {
                let text = toml::to_string_pretty(&theme)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                crate::util::atomic_write(&path, &text)?;
            }
        }
        Ok(())
    }

    pub fn list_theme_names() -> Vec<String> {
        let _ = ensure_default_themes();
        let Ok(dir) = MizpahConfig::themes_dir() else {
            return vec!["default".into(), "slate".into()];
        };
        if !dir.is_dir() {
            return vec!["default".into(), "slate".into()];
        }
        let mut names = Vec::new();
        if let Ok(rd) = fs::read_dir(dir) {
            for ent in rd.flatten() {
                let path = ent.path();
                if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        names.push(stem.to_string());
                    }
                }
            }
        }
        names.sort();
        if names.is_empty() {
            names.push("default".into());
            names.push("slate".into());
        }
        names
    }

    pub fn load_theme(name: &str) -> Theme {
        let _ = ensure_default_themes();
        if let Ok(dir) = MizpahConfig::themes_dir() {
            let path = dir.join(format!("{name}.toml"));
            if let Ok(text) = fs::read_to_string(path) {
                if let Ok(t) = toml::from_str::<Theme>(&text) {
                    return t;
                }
            }
        }
        if name == "slate" {
            slate_theme()
        } else {
            default_theme()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::env_lock;

    fn with_isolated_config_dir<F: FnOnce(&std::path::Path)>(suffix: &str, f: F) {
        let _guard = env_lock();
        let dir = std::env::temp_dir().join(format!(
            "mizpah-keymap-{suffix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let old = std::env::var_os("MIZPAH_CONFIG_DIR");
        std::env::set_var("MIZPAH_CONFIG_DIR", &dir);
        f(&dir);
        match old {
            Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
            None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn defaults_match_expected() {
        let k = Keymap::default();
        assert_eq!(k.next_error, "e");
        assert_eq!(k.prev_error, "E");
        assert_eq!(k.down, "j");
        assert_eq!(k.up, "k");
        assert_eq!(k.show_trace, "t");
        assert_eq!(k.quit, "q");
    }

    #[test]
    fn theme_defaults_named() {
        let t = themes::default_theme();
        assert_eq!(t.name, "default");
        assert!(!t.accent.is_empty());
        let slate = themes::slate_theme();
        assert_eq!(slate.name, "slate");
        assert_ne!(slate.accent, t.accent);
    }

    #[test]
    fn load_missing_keymap_uses_defaults() {
        with_isolated_config_dir("missing", |_dir| {
            assert_eq!(Keymap::load(), Keymap::default());
        });
    }

    #[test]
    fn load_custom_keymap_from_file() {
        with_isolated_config_dir("custom", |dir| {
            fs::write(
                dir.join("keymaps.toml"),
                r#"nextError = "n"
prevError = "p"
down = "down"
up = "up"
quit = "x"
showTrace = "r"
"#,
            )
            .unwrap();
            let k = Keymap::load();
            assert_eq!(k.next_error, "n");
            assert_eq!(k.prev_error, "p");
            assert_eq!(k.quit, "x");
            assert_eq!(k.show_trace, "r");
        });
    }

    #[test]
    fn load_invalid_keymap_falls_back_to_defaults() {
        with_isolated_config_dir("invalid", |dir| {
            fs::write(dir.join("keymaps.toml"), "[[[bad").unwrap();
            assert_eq!(Keymap::load(), Keymap::default());
        });
    }

    #[test]
    fn ensure_default_file_creates_keymaps_toml() {
        with_isolated_config_dir("ensure", |dir| {
            Keymap::ensure_default_file().unwrap();
            let path = dir.join("keymaps.toml");
            assert!(path.is_file());
            Keymap::ensure_default_file().unwrap();
            let text = fs::read_to_string(path).unwrap();
            assert!(text.contains("nextError"));
        });
    }

    #[test]
    fn themes_ensure_and_list() {
        with_isolated_config_dir("themes", |dir| {
            themes::ensure_default_themes().unwrap();
            let names = themes::list_theme_names();
            assert!(names.contains(&"default".to_string()));
            assert!(names.contains(&"slate".to_string()));
            assert!(dir.join("themes/default.toml").is_file());
            assert!(dir.join("themes/slate.toml").is_file());
        });
    }

    #[test]
    fn load_theme_by_name_and_fallback() {
        with_isolated_config_dir("load-theme", |dir| {
            themes::ensure_default_themes().unwrap();
            let custom = themes::Theme {
                name: "neon".into(),
                background: "#000".into(),
                foreground: "#fff".into(),
                accent: "#0ff".into(),
                muted: "#888".into(),
                error: "#f00".into(),
                warn: "#ff0".into(),
            };
            fs::write(
                dir.join("themes/neon.toml"),
                toml::to_string_pretty(&custom).unwrap(),
            )
            .unwrap();
            let loaded = themes::load_theme("neon");
            assert_eq!(loaded.accent, "#0ff");
            assert_eq!(themes::load_theme("slate").name, "slate");
            assert_eq!(themes::load_theme("unknown").name, "default");
        });
    }

    #[test]
    fn list_theme_names_when_dir_missing_uses_builtins() {
        with_isolated_config_dir("list-fallback", |_dir| {
            let names = themes::list_theme_names();
            assert!(names.contains(&"default".to_string()));
            assert!(names.contains(&"slate".to_string()));
        });
    }

    #[test]
    fn load_keymap_read_error_uses_defaults() {
        with_isolated_config_dir("read-err", |dir| {
            let path = dir.join("keymaps.toml");
            fs::create_dir(&path).unwrap();
            assert_eq!(Keymap::load(), Keymap::default());
        });
    }

    #[test]
    fn list_theme_names_empty_dir_falls_back_to_builtins() {
        with_isolated_config_dir("empty-themes", |dir| {
            fs::create_dir_all(dir.join("themes")).unwrap();
            let names = themes::list_theme_names();
            assert_eq!(names, vec!["default".to_string(), "slate".to_string()]);
        });
    }

    #[test]
    fn load_theme_invalid_toml_falls_back() {
        with_isolated_config_dir("bad-theme", |dir| {
            themes::ensure_default_themes().unwrap();
            fs::write(dir.join("themes/broken.toml"), "[[[").unwrap();
            assert_eq!(themes::load_theme("broken").name, "default");
            assert_eq!(themes::load_theme("slate").name, "slate");
        });
    }

    #[test]
    fn theme_default_impl_matches_default_theme() {
        assert_eq!(themes::Theme::default().name, themes::default_theme().name);
    }
}
