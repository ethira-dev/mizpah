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
}

pub fn http_client() -> Result<reqwest::Client, String> {
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
    if lines.is_empty() {
        return Ok(());
    }
    let body = BatchBody {
        service,
        cmd,
        mzp,
        lines,
    };
    let resp = client
        .post(url)
        .json(&body)
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
