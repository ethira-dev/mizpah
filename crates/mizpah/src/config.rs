//! Hub configuration loaded from the Mizpah config directory.

use crate::util::{atomic_write, config_dir};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::PathBuf;

/// Default opid / trace field names (Phase C).
pub const DEFAULT_TRACE_FIELDS: &[&str] = &[
    "trace_id",
    "traceId",
    "request_id",
    "requestId",
    "correlation_id",
    "correlationId",
    "span_id",
    "spanId",
    "opid",
];

/// Opt-in OIDC / token auth for shared hubs. Disabled by default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AuthConfig {
    /// When false (default), the hub stays unauthenticated.
    pub enabled: bool,
    /// OIDC issuer URL (discovery base).
    pub issuer_url: String,
    /// Confidential OIDC client id.
    pub client_id: String,
    /// Client secret (prefer `MIZPAH_OIDC_CLIENT_SECRET` env).
    pub client_secret: String,
    /// Registered redirect URI (must match IdP app config).
    pub redirect_uri: String,
    /// OIDC scopes requested at login.
    pub scopes: Vec<String>,
    /// Exact email allowlist (union with [`Self::allowed_domains`]).
    pub allowed_emails: Vec<String>,
    /// Email domain allowlist (e.g. `example.com`).
    pub allowed_domains: Vec<String>,
    /// Bearer token for machine ingest (prefer `MIZPAH_INGEST_TOKEN`).
    pub ingest_token: String,
    /// Bearer token for MCP / API query (prefer `MIZPAH_API_TOKEN`).
    pub api_token: String,
    /// Session cookie lifetime in hours.
    pub session_ttl_hours: u64,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            issuer_url: String::new(),
            client_id: String::new(),
            client_secret: String::new(),
            redirect_uri: String::new(),
            scopes: vec![
                "openid".into(),
                "profile".into(),
                "email".into(),
            ],
            allowed_emails: Vec::new(),
            allowed_domains: Vec::new(),
            ingest_token: String::new(),
            api_token: String::new(),
            session_ttl_hours: 12,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MizpahConfig {
    /// Default bind host when CLI omits `--host`.
    pub host: String,
    /// Default bind / connect port when CLI omits `--port`.
    pub port: u16,
    /// Default max ring buffer bytes.
    pub max_bytes: u64,
    /// Default TTL hours (`0` = disabled).
    pub ttl_hours: u64,
    /// Field names used to resolve a trace / opid.
    pub trace_fields: Vec<String>,
    /// Optional persist directory (Phase K). Relative paths resolve under config dir.
    pub persist_dir: Option<String>,
    /// Refuse remote/SSH ingest helpers when set (Phase L).
    pub secure: bool,
    /// Optional OIDC / token auth (see [`AuthConfig`]).
    pub auth: AuthConfig,
}

impl Default for MizpahConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: crate::hub::DEFAULT_PORT,
            max_bytes: crate::store::DEFAULT_MAX_BYTES,
            ttl_hours: crate::store::DEFAULT_TTL_HOURS,
            trace_fields: DEFAULT_TRACE_FIELDS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            persist_dir: None,
            secure: false,
            auth: AuthConfig::default(),
        }
    }
}

impl MizpahConfig {
    pub fn config_file_path() -> io::Result<PathBuf> {
        Ok(config_dir()?.join("config.toml"))
    }

    pub fn formats_dir() -> io::Result<PathBuf> {
        Ok(config_dir()?.join("formats"))
    }

    pub fn themes_dir() -> io::Result<PathBuf> {
        Ok(config_dir()?.join("themes"))
    }

    pub fn scripts_dir() -> io::Result<PathBuf> {
        Ok(config_dir()?.join("scripts"))
    }

    pub fn keymaps_path() -> io::Result<PathBuf> {
        Ok(config_dir()?.join("keymaps.toml"))
    }

    /// Ensure config dir layout exists (`formats/`, `themes/`, `scripts/`).
    pub fn ensure_layout() -> io::Result<PathBuf> {
        let root = config_dir()?;
        fs::create_dir_all(&root)?;
        fs::create_dir_all(Self::formats_dir()?)?;
        fs::create_dir_all(Self::themes_dir()?)?;
        fs::create_dir_all(Self::scripts_dir()?)?;
        Ok(root)
    }

    /// Load config from disk, or defaults if missing/unreadable.
    pub fn load() -> Self {
        match Self::try_load() {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(error = %e, "using default Mizpah config");
                Self::default()
            }
        }
    }

    pub fn try_load() -> io::Result<Self> {
        let path = Self::config_file_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path)?;
        toml::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Write current config (pretty TOML) to the config file.
    pub fn save(&self) -> io::Result<()> {
        Self::ensure_layout()?;
        let path = Self::config_file_path()?;
        let text = toml::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        atomic_write(&path, &text)
    }

    /// Resolve persist dir if configured.
    pub fn resolve_persist_dir(&self) -> Option<PathBuf> {
        let raw = self.persist_dir.as_deref()?.trim();
        if raw.is_empty() {
            return None;
        }
        let p = PathBuf::from(raw);
        if p.is_absolute() {
            Some(p)
        } else {
            config_dir().ok().map(|root| root.join(p))
        }
    }
}

/// Write a default config.toml when none exists.
pub fn ensure_default_config_file() -> io::Result<PathBuf> {
    MizpahConfig::ensure_layout()?;
    let path = MizpahConfig::config_file_path()?;
    if !path.exists() {
        MizpahConfig::default().save()?;
    }
    Ok(path)
}

/// Merge: CLI / explicit values win over config file for hub bind defaults.
#[allow(clippy::too_many_arguments)]
pub fn apply_hub_defaults(
    host: String,
    port: u16,
    max_bytes: u64,
    ttl_hours: u64,
    cli_host_is_default: bool,
    cli_port_is_default: bool,
    cli_max_is_default: bool,
    cli_ttl_is_default: bool,
) -> (String, u16, u64, u64) {
    let cfg = MizpahConfig::load();
    let host = if cli_host_is_default { cfg.host } else { host };
    let port = if cli_port_is_default { cfg.port } else { port };
    let max_bytes = if cli_max_is_default {
        cfg.max_bytes
    } else {
        max_bytes
    };
    let ttl_hours = if cli_ttl_is_default {
        cfg.ttl_hours
    } else {
        ttl_hours
    };
    (host, port, max_bytes, ttl_hours)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::env_lock;

    fn with_isolated_config_dir<F: FnOnce(&std::path::Path)>(suffix: &str, f: F) {
        let _guard = env_lock();
        let dir = std::env::temp_dir().join(format!(
            "mizpah-config-{suffix}-{}-{}",
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
    fn roundtrip_toml() {
        let c = MizpahConfig {
            port: 4000,
            ..Default::default()
        };
        let s = toml::to_string_pretty(&c).unwrap();
        let back: MizpahConfig = toml::from_str(&s).unwrap();
        assert_eq!(back.port, 4000);
        assert!(!back.trace_fields.is_empty());
        assert!(!back.auth.enabled);
    }

    #[test]
    fn auth_section_roundtrip() {
        let c = MizpahConfig {
            auth: AuthConfig {
                enabled: true,
                issuer_url: "https://idp.example".into(),
                client_id: "mizpah".into(),
                client_secret: "s".into(),
                redirect_uri: "https://logs.example/api/auth/callback".into(),
                allowed_domains: vec!["example.com".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let s = toml::to_string_pretty(&c).unwrap();
        assert!(s.contains("[auth]"));
        assert!(s.contains("issuerUrl"));
        let back: MizpahConfig = toml::from_str(&s).unwrap();
        assert!(back.auth.enabled);
        assert_eq!(back.auth.issuer_url, "https://idp.example");
        assert_eq!(back.auth.allowed_domains, vec!["example.com".to_string()]);
    }

    #[test]
    fn load_missing_is_default() {
        with_isolated_config_dir("missing", |_dir| {
            assert_eq!(MizpahConfig::try_load().unwrap(), MizpahConfig::default());
            assert_eq!(MizpahConfig::load(), MizpahConfig::default());
        });
    }

    #[test]
    fn try_load_invalid_toml_falls_back_via_load() {
        with_isolated_config_dir("invalid", |dir| {
            fs::write(dir.join("config.toml"), "not valid [[toml").unwrap();
            assert!(MizpahConfig::try_load().is_err());
            assert_eq!(MizpahConfig::load(), MizpahConfig::default());
        });
    }

    #[test]
    fn save_and_try_load_roundtrip() {
        with_isolated_config_dir("save", |dir| {
            let cfg = MizpahConfig {
                host: "0.0.0.0".into(),
                port: 5001,
                max_bytes: 2048,
                ttl_hours: 12,
                persist_dir: Some("data/spill".into()),
                secure: true,
                ..Default::default()
            };
            cfg.save().unwrap();
            let loaded = MizpahConfig::try_load().unwrap();
            assert_eq!(loaded.host, "0.0.0.0");
            assert_eq!(loaded.port, 5001);
            assert_eq!(loaded.max_bytes, 2048);
            assert_eq!(loaded.ttl_hours, 12);
            assert_eq!(loaded.persist_dir.as_deref(), Some("data/spill"));
            assert!(loaded.secure);
            assert!(dir.join("formats").is_dir());
            assert!(dir.join("themes").is_dir());
            assert!(dir.join("scripts").is_dir());
        });
    }

    #[test]
    fn ensure_layout_creates_dirs() {
        with_isolated_config_dir("layout", |dir| {
            MizpahConfig::ensure_layout().unwrap();
            assert!(MizpahConfig::formats_dir().unwrap().is_dir());
            assert!(MizpahConfig::themes_dir().unwrap().is_dir());
            assert!(MizpahConfig::scripts_dir().unwrap().is_dir());
            assert!(MizpahConfig::keymaps_path().unwrap().starts_with(dir));
            assert!(MizpahConfig::config_file_path()
                .unwrap()
                .ends_with("config.toml"));
        });
    }

    #[test]
    fn ensure_default_config_file_writes_once() {
        with_isolated_config_dir("default-file", |dir| {
            let path = ensure_default_config_file().unwrap();
            assert_eq!(path, dir.join("config.toml"));
            assert!(path.is_file());
            let first = fs::read_to_string(&path).unwrap();
            let _ = ensure_default_config_file().unwrap();
            let second = fs::read_to_string(&path).unwrap();
            assert_eq!(first, second);
        });
    }

    #[test]
    fn resolve_persist_dir_variants() {
        with_isolated_config_dir("persist", |dir| {
            let none = MizpahConfig::default();
            assert!(none.resolve_persist_dir().is_none());

            let empty = MizpahConfig {
                persist_dir: Some("   ".into()),
                ..Default::default()
            };
            assert!(empty.resolve_persist_dir().is_none());

            let relative = MizpahConfig {
                persist_dir: Some("spill".into()),
                ..Default::default()
            };
            assert_eq!(relative.resolve_persist_dir().unwrap(), dir.join("spill"));

            let absolute = MizpahConfig {
                persist_dir: Some("/tmp/mizpah-spill".into()),
                ..Default::default()
            };
            assert_eq!(
                absolute.resolve_persist_dir().unwrap(),
                PathBuf::from("/tmp/mizpah-spill")
            );
        });
    }

    #[test]
    fn apply_hub_defaults_merges_from_config() {
        with_isolated_config_dir("hub-defaults", |dir| {
            let cfg = MizpahConfig {
                host: "10.0.0.1".into(),
                port: 4242,
                max_bytes: 8192,
                ttl_hours: 48,
                ..Default::default()
            };
            fs::create_dir_all(dir).unwrap();
            fs::write(
                dir.join("config.toml"),
                toml::to_string_pretty(&cfg).unwrap(),
            )
            .unwrap();

            let (host, port, max_bytes, ttl) =
                apply_hub_defaults("127.0.0.1".into(), 3149, 1024, 0, true, true, true, true);
            assert_eq!(host, "10.0.0.1");
            assert_eq!(port, 4242);
            assert_eq!(max_bytes, 8192);
            assert_eq!(ttl, 48);

            let (host, port, max_bytes, ttl) = apply_hub_defaults(
                "192.168.1.2".into(),
                9000,
                2048,
                6,
                false,
                false,
                false,
                false,
            );
            assert_eq!(host, "192.168.1.2");
            assert_eq!(port, 9000);
            assert_eq!(max_bytes, 2048);
            assert_eq!(ttl, 6);
        });
    }

    #[test]
    fn config_path_helpers_under_isolated_dir() {
        with_isolated_config_dir("paths", |dir| {
            MizpahConfig::ensure_layout().unwrap();
            assert_eq!(
                MizpahConfig::config_file_path().unwrap(),
                dir.join("config.toml")
            );
            assert_eq!(MizpahConfig::formats_dir().unwrap(), dir.join("formats"));
            assert_eq!(MizpahConfig::themes_dir().unwrap(), dir.join("themes"));
            assert_eq!(MizpahConfig::scripts_dir().unwrap(), dir.join("scripts"));
            assert_eq!(
                MizpahConfig::keymaps_path().unwrap(),
                dir.join("keymaps.toml")
            );
        });
    }

    #[test]
    fn resolve_persist_dir_none_when_unset() {
        let cfg = MizpahConfig {
            persist_dir: None,
            ..Default::default()
        };
        assert!(cfg.resolve_persist_dir().is_none());
    }
}
