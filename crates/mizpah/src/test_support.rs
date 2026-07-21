//! Shared helpers for integration tests (ephemeral hub, etc.).

use std::sync::{Mutex, MutexGuard};

/// Opaque guard so clippy does not flag intentional std mutex holds across `.await`
/// in tests that serialize process-global env mutation.
pub(crate) struct EnvLock {
    _guard: MutexGuard<'static, ()>,
}

/// Serialize tests that mutate process-global env vars (`HOME`, `MIZPAH_CONFIG_DIR`, …).
pub(crate) fn env_lock() -> EnvLock {
    static LOCK: Mutex<()> = Mutex::new(());
    EnvLock {
        _guard: LOCK.lock().unwrap_or_else(|e| e.into_inner()),
    }
}

/// Opaque guard for terminal-opener injection tests (same rationale as [`EnvLock`]).
pub(crate) struct TerminalOpenerLock {
    _guard: MutexGuard<'static, ()>,
}

/// Serialize tests that inject a global terminal opener (`investigate::set_test_terminal_opener`).
pub(crate) fn terminal_opener_lock() -> TerminalOpenerLock {
    static LOCK: Mutex<()> = Mutex::new(());
    TerminalOpenerLock {
        _guard: LOCK.lock().unwrap_or_else(|e| e.into_inner()),
    }
}

#[cfg(test)]
pub(crate) mod hub {
    use crate::api::{self, AppState};
    use crate::store::Store;
    use crate::update;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::net::TcpListener;

    /// Bind an ephemeral loopback hub and return its base URL and store.
    pub async fn spawn_test_hub() -> (String, Arc<Store>) {
        let store = Arc::new(Store::new(1024 * 1024));
        let state = AppState {
            store: Arc::clone(&store),
            project_dir: std::env::temp_dir(),
            update: update::UpdateManager::new(update::RestartContext {
                host: "127.0.0.1".into(),
                port: 0,
                project_dir: std::env::temp_dir(),
                max_bytes: 1024 * 1024,
                ttl_hours: 0,
            }),
            auth: None,
        };
        let app = api::router(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        (format!("http://{addr}"), store)
    }
}

#[cfg(test)]
pub(crate) use hub::spawn_test_hub;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_lock_acquires_and_releases() {
        let guard = env_lock();
        drop(guard);
        let _again = env_lock();
    }

    #[test]
    fn terminal_opener_lock_acquires() {
        let guard = terminal_opener_lock();
        drop(guard);
        let _again = terminal_opener_lock();
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn spawn_test_hub_serves() {
        let (base, store) = spawn_test_hub().await;
        store.push_line("api", r#"{"msg":"hi"}"#).await;
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{base}/api/health"))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
    }
}
