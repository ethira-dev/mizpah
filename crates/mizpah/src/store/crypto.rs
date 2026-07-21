//! Zero-config at-rest encryption for persist segments and update spill.
//!
//! DEK is loaded from the OS credential store when available, otherwise a
//! machine/user-bound sealed key file under the config directory.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use hkdf::Hkdf;
use sha2::Sha256;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::Arc;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const KEYRING_SERVICE: &str = "dev.ethira.mizpah";
const KEYRING_USER: &str = "log-store-dek";
const DEK_FILE: &str = "log-store.dek";
const SALT_FILE: &str = "log-store.salt";
const SEAL_VERSION: u8 = 1;
const NONCE_LEN: usize = 12;
const DEK_LEN: usize = 32;
const LINE_PREFIX: &str = "mzp1:";
const HKDF_INFO: &[u8] = b"mizpah-log-store-dek-v1";

/// Wire format: `version (1) || nonce (12) || ciphertext||tag`.
pub const SEAL_OVERHEAD: usize = 1 + NONCE_LEN + 16;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Msg(String),
}

impl CryptoError {
    fn msg(s: impl Into<String>) -> Self {
        Self::Msg(s.into())
    }
}

#[derive(Zeroize, ZeroizeOnDrop)]
struct DekBytes([u8; DEK_LEN]);

/// Process-wide log-store data encryption key.
pub struct LogCrypto {
    dek: DekBytes,
}

impl std::fmt::Debug for LogCrypto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LogCrypto(<redacted>)")
    }
}

impl LogCrypto {
    /// Load or create the DEK for `config_dir` (OS keychain, then file fallback).
    pub fn load_or_create(config_dir: &Path) -> Result<Arc<Self>, CryptoError> {
        ensure_secure_dir(config_dir)?;

        if !force_file_dek() {
            match load_from_keyring() {
                Ok(Some(dek)) => {
                    mlock_best_effort(&dek.0);
                    return Ok(Arc::new(Self { dek }));
                }
                Ok(None) => {
                    let dek = generate_dek()?;
                    if store_in_keyring(&dek).is_ok() {
                        mlock_best_effort(&dek.0);
                        return Ok(Arc::new(Self { dek }));
                    }
                    // Fall through to file-backed storage.
                    let crypto = Self::persist_file_dek(config_dir, dek)?;
                    tracing::warn!(
                        "OS keychain unavailable; using file-backed log-store key under {}",
                        config_dir.display()
                    );
                    return Ok(crypto);
                }
                Err(e) => {
                    tracing::debug!(error = %e, "keyring load failed; trying file-backed DEK");
                }
            }
        }

        Self::load_or_create_file(config_dir)
    }

    fn load_or_create_file(config_dir: &Path) -> Result<Arc<Self>, CryptoError> {
        ensure_secure_dir(config_dir)?;
        let dek_path = config_dir.join(DEK_FILE);
        let salt_path = config_dir.join(SALT_FILE);
        if dek_path.exists() && salt_path.exists() {
            let dek = unwrap_file_dek(config_dir)?;
            mlock_best_effort(&dek.0);
            return Ok(Arc::new(Self { dek }));
        }
        let dek = generate_dek()?;
        let crypto = Self::persist_file_dek(config_dir, dek)?;
        if !force_file_dek() {
            tracing::warn!(
                "using file-backed log-store key; OS keychain unavailable ({})",
                config_dir.display()
            );
        }
        Ok(crypto)
    }

    fn persist_file_dek(config_dir: &Path, dek: DekBytes) -> Result<Arc<Self>, CryptoError> {
        let salt = random_bytes::<DEK_LEN>()?;
        let wrapped = wrap_dek_with_salt(&dek.0, &salt)?;
        write_private_bytes(&config_dir.join(SALT_FILE), &salt)?;
        write_private_bytes(&config_dir.join(DEK_FILE), &wrapped)?;
        mlock_best_effort(&dek.0);
        Ok(Arc::new(Self { dek }))
    }

    /// Seal plaintext into versioned binary (`version || nonce || ct||tag`).
    pub fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let cipher = Aes256Gcm::new_from_slice(&self.dek.0)
            .map_err(|e| CryptoError::msg(format!("aes key: {e}")))?;
        let mut nonce_bytes = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce_bytes)
            .map_err(|e| CryptoError::msg(format!("getrandom: {e}")))?;
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher
            .encrypt(nonce, plaintext)
            .map_err(|_| CryptoError::msg("encrypt failed"))?;
        let mut out = Vec::with_capacity(SEAL_OVERHEAD + plaintext.len());
        out.push(SEAL_VERSION);
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Open a sealed blob produced by [`Self::seal`].
    pub fn open(&self, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
        if sealed.len() < SEAL_OVERHEAD {
            return Err(CryptoError::msg("sealed blob too short"));
        }
        if sealed[0] != SEAL_VERSION {
            return Err(CryptoError::msg(format!(
                "unsupported seal version {}",
                sealed[0]
            )));
        }
        let nonce = Nonce::from_slice(&sealed[1..1 + NONCE_LEN]);
        let ct = &sealed[1 + NONCE_LEN..];
        let cipher = Aes256Gcm::new_from_slice(&self.dek.0)
            .map_err(|e| CryptoError::msg(format!("aes key: {e}")))?;
        let plain = cipher
            .decrypt(nonce, ct)
            .map_err(|_| CryptoError::msg("decrypt/auth failed"))?;
        Ok(Zeroizing::new(plain))
    }

    /// Seal to a single-line persist/spill record (`mzp1:` + base64).
    pub fn seal_line(&self, plaintext: &[u8]) -> Result<String, CryptoError> {
        let sealed = self.seal(plaintext)?;
        Ok(format!("{LINE_PREFIX}{}", B64.encode(sealed)))
    }

    /// Open a `mzp1:` line, or return `None` if the line is not sealed format.
    pub fn open_line(&self, line: &str) -> Result<Option<Zeroizing<Vec<u8>>>, CryptoError> {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix(LINE_PREFIX) else {
            return Ok(None);
        };
        let sealed = B64
            .decode(rest.trim())
            .map_err(|e| CryptoError::msg(format!("invalid sealed line: {e}")))?;
        Ok(Some(self.open(&sealed)?))
    }

    #[cfg(test)]
    pub fn from_bytes_for_test(bytes: [u8; DEK_LEN]) -> Arc<Self> {
        let dek = DekBytes(bytes);
        mlock_best_effort(&dek.0);
        Arc::new(Self { dek })
    }
}

/// True when a trimmed line looks like legacy plaintext JSON persist/spill.
pub fn looks_like_legacy_json_line(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('{') || t.starts_with('[')
}

/// True when a trimmed line is an encrypted `mzp1:` record.
pub fn looks_like_sealed_line(line: &str) -> bool {
    line.trim().starts_with(LINE_PREFIX)
}

fn force_file_dek() -> bool {
    // Tests and explicit opt-in avoid interactive OS keychain prompts.
    cfg!(test) || std::env::var_os("MIZPAH_USE_FILE_DEK").is_some()
}

fn generate_dek() -> Result<DekBytes, CryptoError> {
    Ok(DekBytes(random_bytes()?))
}

fn random_bytes<const N: usize>() -> Result<[u8; N], CryptoError> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).map_err(|e| CryptoError::msg(format!("getrandom: {e}")))?;
    Ok(buf)
}

fn load_from_keyring() -> Result<Option<DekBytes>, CryptoError> {
    #[cfg(target_os = "macos")]
    {
        match macos::load_dek() {
            Ok(Some(dek)) => return Ok(Some(dek)),
            Ok(None) => {}
            Err(e) => tracing::debug!(error = %e, "macos keychain load failed"),
        }
    }

    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .map_err(|e| CryptoError::msg(format!("keyring: {e}")))?;
    match entry.get_secret() {
        Ok(secret) => {
            if secret.len() != DEK_LEN {
                return Err(CryptoError::msg("keyring DEK has wrong length"));
            }
            let mut arr = [0u8; DEK_LEN];
            arr.copy_from_slice(&secret);
            Ok(Some(DekBytes(arr)))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(CryptoError::msg(format!("keyring get: {e}"))),
    }
}

fn store_in_keyring(dek: &DekBytes) -> Result<(), CryptoError> {
    #[cfg(target_os = "macos")]
    {
        if macos::store_dek(&dek.0).is_ok() {
            return Ok(());
        }
    }

    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .map_err(|e| CryptoError::msg(format!("keyring: {e}")))?;
    entry
        .set_secret(&dek.0)
        .map_err(|e| CryptoError::msg(format!("keyring set: {e}")))
}

fn unwrap_file_dek(config_dir: &Path) -> Result<DekBytes, CryptoError> {
    let salt = read_private_bytes(&config_dir.join(SALT_FILE))?;
    let wrapped = read_private_bytes(&config_dir.join(DEK_FILE))?;
    if salt.len() != DEK_LEN {
        return Err(CryptoError::msg("invalid salt length"));
    }
    let mut salt_arr = [0u8; DEK_LEN];
    salt_arr.copy_from_slice(&salt);
    let wrap_key = derive_wrap_key(&salt_arr)?;
    let cipher = Aes256Gcm::new_from_slice(&wrap_key)
        .map_err(|e| CryptoError::msg(format!("wrap key: {e}")))?;
    if wrapped.len() < NONCE_LEN + 16 {
        return Err(CryptoError::msg("wrapped DEK too short"));
    }
    let nonce = Nonce::from_slice(&wrapped[..NONCE_LEN]);
    let plain = cipher
        .decrypt(nonce, &wrapped[NONCE_LEN..])
        .map_err(|_| CryptoError::msg("unwrap DEK failed"))?;
    if plain.len() != DEK_LEN {
        return Err(CryptoError::msg("unwrapped DEK wrong length"));
    }
    let mut arr = [0u8; DEK_LEN];
    arr.copy_from_slice(&plain);
    Ok(DekBytes(arr))
}

fn wrap_dek_with_salt(dek: &[u8; DEK_LEN], salt: &[u8; DEK_LEN]) -> Result<Vec<u8>, CryptoError> {
    let wrap_key = derive_wrap_key(salt)?;
    let cipher = Aes256Gcm::new_from_slice(&wrap_key)
        .map_err(|e| CryptoError::msg(format!("wrap key: {e}")))?;
    let nonce_bytes = random_bytes::<NONCE_LEN>()?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, dek.as_ref())
        .map_err(|_| CryptoError::msg("wrap DEK failed"))?;
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

fn derive_wrap_key(salt: &[u8; DEK_LEN]) -> Result<[u8; DEK_LEN], CryptoError> {
    let mut ikm = Vec::with_capacity(DEK_LEN + 64);
    ikm.extend_from_slice(salt);
    ikm.extend_from_slice(local_binding_material().as_bytes());
    let hk = Hkdf::<Sha256>::new(Some(salt.as_slice()), &ikm);
    let mut okm = [0u8; DEK_LEN];
    hk.expand(HKDF_INFO, &mut okm)
        .map_err(|_| CryptoError::msg("hkdf expand failed"))?;
    ikm.zeroize();
    Ok(okm)
}

fn local_binding_material() -> String {
    let uid = {
        #[cfg(unix)]
        {
            // SAFETY: getuid has no preconditions.
            unsafe { libc::getuid() }.to_string()
        }
        #[cfg(not(unix))]
        {
            std::env::var("USERNAME")
                .or_else(|_| std::env::var("USER"))
                .unwrap_or_else(|_| "user".into())
        }
    };
    let host = hostname_best_effort();
    format!("uid={uid};host={host}")
}

fn hostname_best_effort() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| {
            #[cfg(unix)]
            {
                let mut buf = [0u8; 256];
                // SAFETY: gethostname writes into a valid buffer.
                let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
                if rc == 0 {
                    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                    return String::from_utf8_lossy(&buf[..len]).into_owned();
                }
            }
            "unknown".into()
        })
}

fn ensure_secure_dir(dir: &Path) -> Result<(), CryptoError> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn refuse_symlink(path: &Path) -> Result<(), CryptoError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(CryptoError::msg(format!(
            "refusing symlink at {}",
            path.display()
        ))),
        Ok(_) | Err(_) => Ok(()),
    }
}

fn open_new_private_file(path: &Path) -> Result<File, CryptoError> {
    if path.exists() {
        refuse_symlink(path)?;
        let _ = fs::remove_file(path);
    }
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let file = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(file)
}

fn write_private_bytes(path: &Path, bytes: &[u8]) -> Result<(), CryptoError> {
    let tmp = path.with_file_name(format!(
        ".{}.tmp.{}",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("dek"),
        std::process::id()
    ));
    {
        let mut f = open_new_private_file(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    if path.exists() {
        refuse_symlink(path)?;
    }
    fs::rename(&tmp, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn read_private_bytes(path: &Path) -> Result<Vec<u8>, CryptoError> {
    refuse_symlink(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        Ok(buf)
    }
    #[cfg(not(unix))]
    {
        Ok(fs::read(path)?)
    }
}

fn mlock_best_effort(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    #[cfg(unix)]
    {
        // SAFETY: mlock on a valid slice; failure is ignored.
        let _ = unsafe { libc::mlock(bytes.as_ptr().cast(), bytes.len()) };
    }
}

#[cfg(target_os = "macos")]
mod macos {
    //! macOS Keychain via Security framework (app-bound when the binary is signed).
    //!
    //! The DEK lives in the login keychain under a fixed service/account, not next
    //! to ciphertext. Release builds should be code-signed + hardened-runtime so
    //! other same-user processes cannot casually extract Keychain items for this
    //! app. Ad-hoc `cargo run` builds are weaker by nature.

    use super::{CryptoError, DekBytes, DEK_LEN, KEYRING_SERVICE, KEYRING_USER};
    use security_framework::os::macos::passwords::find_generic_password;
    use security_framework::passwords::{delete_generic_password, set_generic_password};

    pub fn store_dek(dek: &[u8; DEK_LEN]) -> Result<(), CryptoError> {
        let _ = delete_generic_password(KEYRING_SERVICE, KEYRING_USER);
        set_generic_password(KEYRING_SERVICE, KEYRING_USER, dek)
            .map_err(|e| CryptoError::msg(format!("macos keychain set: {e}")))?;
        Ok(())
    }

    pub fn load_dek() -> Result<Option<DekBytes>, CryptoError> {
        match find_generic_password(None, KEYRING_SERVICE, KEYRING_USER) {
            Ok((password, _)) => {
                if password.len() != DEK_LEN {
                    return Err(CryptoError::msg("macos keychain DEK wrong length"));
                }
                let mut arr = [0u8; DEK_LEN];
                arr.copy_from_slice(&password);
                Ok(Some(DekBytes(arr)))
            }
            Err(_) => Ok(None),
        }
    }
}

/// Load crypto rooted at an explicit directory (config dir or test path).
pub fn load_log_crypto_at(dir: &Path) -> Result<Arc<LogCrypto>, CryptoError> {
    LogCrypto::load_or_create(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip() {
        let crypto = LogCrypto::from_bytes_for_test([7u8; 32]);
        let sealed = crypto.seal(b"hello logs").unwrap();
        let plain = crypto.open(&sealed).unwrap();
        assert_eq!(&plain[..], b"hello logs");
    }

    #[test]
    fn seal_line_roundtrip() {
        let crypto = LogCrypto::from_bytes_for_test([9u8; 32]);
        let line = crypto.seal_line(b"{\"a\":1}").unwrap();
        assert!(looks_like_sealed_line(&line));
        let plain = crypto.open_line(&line).unwrap().unwrap();
        assert_eq!(&plain[..], b"{\"a\":1}");
    }

    #[test]
    fn tampered_seal_fails() {
        let crypto = LogCrypto::from_bytes_for_test([1u8; 32]);
        let mut sealed = crypto.seal(b"secret").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0xff;
        assert!(crypto.open(&sealed).is_err());
    }

    #[test]
    fn file_dek_persists_across_reload() {
        let dir = tempfile::tempdir().unwrap();
        let a = LogCrypto::load_or_create(dir.path()).unwrap();
        let line = a.seal_line(b"persist-me").unwrap();
        let b = LogCrypto::load_or_create(dir.path()).unwrap();
        let plain = b.open_line(&line).unwrap().unwrap();
        assert_eq!(&plain[..], b"persist-me");
        assert!(dir.path().join(DEK_FILE).exists());
        assert!(dir.path().join(SALT_FILE).exists());
    }

    #[test]
    fn debug_redacts_dek() {
        let crypto = LogCrypto::from_bytes_for_test([0u8; 32]);
        let s = format!("{crypto:?}");
        assert!(s.contains("redacted"));
        assert!(!s.contains("0, 0, 0"));
    }
}
