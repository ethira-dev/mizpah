//! Shared HTTP batch ingest client used by shell and browser forwarders.

use crate::mzp_meta::MzpMeta;
use serde::Serialize;
use std::time::Duration;
use tracing::warn;

pub const QUEUE_CAPACITY: usize = 4096;
pub const BATCH_MAX: usize = 128;
pub const BATCH_FLUSH: Duration = Duration::from_millis(50);
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(3);
pub const MAX_BACKOFF: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub enum BatchError {
    Disconnected,
    Other(String),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchBody<'a> {
    service: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cmd: Option<&'a str>,
    mzp: &'a MzpMeta,
    lines: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    format_hint: Option<&'a str>,
}

pub fn http_client() -> Result<reqwest::Client, String> {
    crate::util::ensure_rustls_crypto_provider();
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())
}

/// POST `/api/ingest/batch`. `cmd` is omitted when `None`.
pub async fn post_batch(
    client: &reqwest::Client,
    url: &str,
    service: &str,
    cmd: Option<&str>,
    mzp: &MzpMeta,
    lines: &[String],
) -> Result<(), BatchError> {
    post_batch_hint(client, url, service, cmd, mzp, lines, None).await
}

/// POST `/api/ingest/batch` with an optional format lock hint.
pub async fn post_batch_hint(
    client: &reqwest::Client,
    url: &str,
    service: &str,
    cmd: Option<&str>,
    mzp: &MzpMeta,
    lines: &[String],
    format_hint: Option<&str>,
) -> Result<(), BatchError> {
    if lines.is_empty() {
        return Ok(());
    }
    let body = BatchBody {
        service,
        cmd,
        mzp,
        lines,
        format_hint,
    };
    let mut req = client.post(url).json(&body);
    if let Ok(token) = std::env::var("MIZPAH_INGEST_TOKEN") {
        let token = token.trim();
        if !token.is_empty() {
            req = req.bearer_auth(token);
        }
    }
    let resp = req
        .send()
        .await
        .map_err(|e| BatchError::Other(e.to_string()))?;
    if resp.status() == reqwest::StatusCode::CONFLICT {
        return Err(BatchError::Disconnected);
    }
    if !resp.status().is_success() {
        return Err(BatchError::Other(format!("status {}", resp.status())));
    }
    Ok(())
}

/// Apply exponential backoff after a failed batch (capped at [`MAX_BACKOFF`]).
pub async fn sleep_backoff(backoff: &mut Duration) {
    tokio::time::sleep(*backoff).await;
    *backoff = (*backoff * 2).min(MAX_BACKOFF);
}

pub fn reset_backoff(backoff: &mut Duration) {
    *backoff = Duration::from_millis(100);
}

/// Drain a channel forever when the HTTP client cannot be built (avoids producer backpressure).
pub async fn drain_on_client_error<T>(
    label: &str,
    err: &str,
    rx: &mut tokio::sync::mpsc::Receiver<T>,
) {
    warn!(error = %err, "{label}: http client failed");
    while rx.recv().await.is_some() {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mzp_meta::MzpMeta;
    use axum::http::StatusCode;
    use axum::routing::post;
    use axum::Router;
    use serde_json::json;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    fn test_mzp() -> MzpMeta {
        MzpMeta {
            cwd: "/tmp".into(),
            user: "test".into(),
            pid: 1,
            exe: "/bin/mizpah".into(),
        }
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn post_batch_empty_lines_is_noop() {
        let client = http_client().unwrap();
        let err = post_batch(
            &client,
            "http://127.0.0.1:1/api/ingest/batch",
            "svc",
            None,
            &test_mzp(),
            &[],
        )
        .await;
        assert!(err.is_ok());
    }

    #[test]
    fn http_client_builds() {
        assert!(http_client().is_ok());
    }

    #[test]
    fn reset_backoff_sets_initial() {
        let mut backoff = Duration::from_secs(5);
        reset_backoff(&mut backoff);
        assert_eq!(backoff, Duration::from_millis(100));
    }

    #[tokio::test]
    async fn sleep_backoff_advances_and_caps() {
        tokio::time::pause();
        let mut backoff = Duration::from_millis(100);
        sleep_backoff(&mut backoff).await;
        assert_eq!(backoff, Duration::from_millis(200));
        for _ in 0..10 {
            sleep_backoff(&mut backoff).await;
        }
        assert_eq!(backoff, MAX_BACKOFF);
    }

    #[tokio::test]
    async fn drain_on_client_error_empties_channel() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        tx.send("a").await.unwrap();
        tx.send("b").await.unwrap();
        drop(tx);
        drain_on_client_error("test", "build failed", &mut rx).await;
        assert!(rx.recv().await.is_none());
    }

    // Real TCP / reqwest sockets are unsupported under Miri.
    #[cfg(not(miri))]
    mod hub {
        use super::*;
        use crate::test_support::hub::spawn_test_hub;

        #[cfg(not(miri))]
        #[tokio::test]
        async fn post_batch_success() {
            let (url, store) = spawn_test_hub().await;
            let client = http_client().unwrap();
            let batch_url = format!("{url}/api/ingest/batch");
            post_batch(
                &client,
                &batch_url,
                "svc",
                Some("cmd"),
                &test_mzp(),
                &["{\"msg\":\"hi\"}".into()],
            )
            .await
            .unwrap();
            let stats = store.stats().await;
            assert_eq!(stats.count, 1);
        }

        #[tokio::test]
        async fn post_batch_unreachable_is_other() {
            let client = http_client().unwrap();
            let err = post_batch(
                &client,
                "http://127.0.0.1:1/api/ingest/batch",
                "svc",
                None,
                &test_mzp(),
                &["line".into()],
            )
            .await
            .unwrap_err();
            assert!(matches!(err, BatchError::Other(_)));
        }

        #[cfg(not(miri))]
        #[tokio::test]
        async fn post_batch_disconnected_on_conflict() {
            let (url, _store) = spawn_test_hub().await;
            let client = http_client().unwrap();
            let batch_url = format!("{url}/api/ingest/batch");
            let disconnect_url = format!("{url}/api/services/disconnect");

            client
                .post(&disconnect_url)
                .json(&json!({"service": "svc"}))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();

            let err = post_batch(
                &client,
                &batch_url,
                "svc",
                None,
                &test_mzp(),
                &["line".into()],
            )
            .await
            .unwrap_err();
            assert!(matches!(err, BatchError::Disconnected));
        }

        async fn spawn_error_hub(status: StatusCode) -> String {
            let app = Router::new().route(
                "/api/ingest/batch",
                post(move || async move { (status, "nope") }),
            );
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr: SocketAddr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            format!("http://{addr}")
        }

        #[tokio::test]
        async fn post_batch_non_success_status() {
            let url = spawn_error_hub(StatusCode::INTERNAL_SERVER_ERROR).await;
            let client = http_client().unwrap();
            let err = post_batch(
                &client,
                &format!("{url}/api/ingest/batch"),
                "svc",
                None,
                &test_mzp(),
                &["line".into()],
            )
            .await
            .unwrap_err();
            assert!(matches!(err, BatchError::Other(ref s) if s.contains("500")));
        }

        #[cfg(not(miri))]
        #[tokio::test]
        async fn post_batch_hint_includes_format() {
            let (url, store) = spawn_test_hub().await;
            let client = http_client().unwrap();
            post_batch_hint(
                &client,
                &format!("{url}/api/ingest/batch"),
                "svc",
                None,
                &test_mzp(),
                &["plain".into()],
                Some("syslog"),
            )
            .await
            .unwrap();
            assert_eq!(store.stats().await.count, 1);
        }
    }
}
