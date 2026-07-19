//! Chrome/Edge CDP bridge: console + network → hub ingest.

mod cdp;
mod launch;
mod map;

#[allow(unused_imports)]
pub use map::{
    encode_body_bytes, encode_body_str, service_from_page_url, should_emit_network,
    should_fetch_body, skip_body_url, EncodedBody,
};

use crate::ingest_forward::{
    self, drain_on_client_error, post_batch, reset_backoff, sleep_backoff, BatchError, BATCH_FLUSH,
    BATCH_MAX, QUEUE_CAPACITY,
};
use crate::mzp_meta::MzpMeta;
use launch::{
    launch_browser, resolve_cdp_ws_url, resolve_cdp_ws_url_for_reconnect, wait_for_cdp,
    DEFAULT_CDP_PORT,
};
use map::{resolve_service, IngestItem};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, warn};

const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct BrowserAttachOpts {
    pub service: Option<String>,
    pub host: String,
    pub port: u16,
    pub cdp_port: u16,
    pub cdp_url: Option<String>,
    pub launch: bool,
    pub all_network: bool,
}

impl Default for BrowserAttachOpts {
    fn default() -> Self {
        Self {
            service: None,
            host: crate::hub::DEFAULT_HOST.to_string(),
            port: crate::hub::DEFAULT_PORT,
            cdp_port: DEFAULT_CDP_PORT,
            cdp_url: None,
            launch: false,
            all_network: false,
        }
    }
}

/// Run browser attach until Ctrl-C.
pub async fn run_browser_attach(opts: BrowserAttachOpts) -> Result<(), String> {
    run_browser_attach_until(opts, tokio::signal::ctrl_c()).await
}

/// Like [`run_browser_attach`] with an injectable interrupt future (tests).
pub async fn run_browser_attach_until<F>(
    opts: BrowserAttachOpts,
    interrupt: F,
) -> Result<(), String>
where
    F: std::future::Future<Output = Result<(), std::io::Error>>,
{
    crate::hub::ensure_hub(&opts.host, opts.port, None, false)
        .await
        .map_err(|e| e.to_string())?;

    if opts.launch {
        let _child = launch_browser(opts.cdp_port)?;
        wait_for_cdp(opts.cdp_port).await?;
    }

    let initial_ws = resolve_cdp_ws_url(&opts).await?;
    run_browser_attach_bridge(opts, initial_ws, interrupt).await
}

/// CDP reconnect loop + ingest forwarder until `interrupt` completes.
pub(crate) async fn run_browser_attach_bridge<F>(
    opts: BrowserAttachOpts,
    initial_ws: String,
    interrupt: F,
) -> Result<(), String>
where
    F: std::future::Future<Output = Result<(), std::io::Error>>,
{
    info!(
        %initial_ws,
        hub = %crate::hub::hub_url(&opts.host, opts.port),
        "browser attach: starting CDP bridge"
    );

    let (tx, rx) = mpsc::channel::<IngestItem>(QUEUE_CAPACITY);
    let hub_host = opts.host.clone();
    let hub_port = opts.port;
    let service_override = opts.service.clone();
    let forwarder = tokio::spawn(async move {
        run_ingest_forwarder(rx, hub_host, hub_port, service_override).await;
    });

    let all_network = opts.all_network;
    let cdp_port = opts.cdp_port;
    let cdp_url_override = opts.cdp_url.clone();

    let bridge = {
        let session_tx = tx.clone();
        async move {
            let mut backoff = Duration::from_millis(200);
            let mut ws_url = initial_ws;
            loop {
                match cdp::run_cdp_session(&ws_url, session_tx.clone(), all_network).await {
                    Ok(()) => warn!("browser attach: CDP session ended; reconnecting"),
                    Err(err) => {
                        warn!(error = %err, "browser attach: CDP session error; reconnecting")
                    }
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
                match resolve_cdp_ws_url_for_reconnect(cdp_port, cdp_url_override.as_deref()).await
                {
                    Ok(url) => {
                        ws_url = url;
                        backoff = Duration::from_millis(200);
                    }
                    Err(err) => warn!(error = %err, "browser attach: CDP endpoint unavailable"),
                }
            }
        }
    };

    tokio::select! {
        _ = bridge => {}
        _ = interrupt => {
            info!("browser attach: interrupted");
        }
    }

    drop(tx);
    let _ = tokio::time::timeout(Duration::from_secs(2), forwarder).await;
    Ok(())
}

async fn run_ingest_forwarder(
    mut rx: mpsc::Receiver<IngestItem>,
    hub_host: String,
    hub_port: u16,
    service_override: Option<String>,
) {
    let client = match ingest_forward::http_client() {
        Ok(c) => c,
        Err(err) => {
            drain_on_client_error("browser attach", &err, &mut rx).await;
            return;
        }
    };
    let receiver = MzpMeta::capture();
    let url = format!(
        "{}/api/ingest/batch",
        crate::hub::hub_url(&hub_host, hub_port)
    );
    let mut buf: Vec<IngestItem> = Vec::new();
    let mut backoff = Duration::from_millis(100);

    loop {
        let item = if buf.is_empty() {
            match rx.recv().await {
                Some(i) => i,
                None => break,
            }
        } else {
            let deadline = tokio::time::Instant::now() + BATCH_FLUSH;
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Some(i)) => i,
                Ok(None) => {
                    flush_grouped(
                        &client,
                        &url,
                        &receiver,
                        &mut buf,
                        service_override.as_deref(),
                        &mut backoff,
                    )
                    .await;
                    break;
                }
                Err(_) => {
                    flush_grouped(
                        &client,
                        &url,
                        &receiver,
                        &mut buf,
                        service_override.as_deref(),
                        &mut backoff,
                    )
                    .await;
                    continue;
                }
            }
        };
        buf.push(item);
        if buf.len() >= BATCH_MAX {
            flush_grouped(
                &client,
                &url,
                &receiver,
                &mut buf,
                service_override.as_deref(),
                &mut backoff,
            )
            .await;
        }
    }
}

async fn flush_grouped(
    client: &reqwest::Client,
    url: &str,
    receiver: &MzpMeta,
    buf: &mut Vec<IngestItem>,
    service_override: Option<&str>,
    backoff: &mut Duration,
) {
    if buf.is_empty() {
        return;
    }
    let mut groups: HashMap<String, Vec<String>> = HashMap::new();
    for item in buf.drain(..) {
        let service = resolve_service(service_override, &item.service);
        groups.entry(service).or_default().push(item.line);
    }
    for (service, mut lines) in groups {
        while !lines.is_empty() {
            let take = lines.len().min(BATCH_MAX);
            let chunk: Vec<String> = lines.drain(..take).collect();
            match post_batch(client, url, &service, None, receiver, &chunk).await {
                Ok(()) => {
                    reset_backoff(backoff);
                }
                Err(BatchError::Disconnected) => {
                    warn!(%service, "browser attach: service disconnected; dropping batch");
                    reset_backoff(backoff);
                }
                Err(BatchError::Other(err)) => {
                    warn!(error = %err, %service, "browser attach: batch ingest failed");
                    sleep_backoff(backoff).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn flush_grouped_with_empty_buffer() {
        let (hub_url, _store) = crate::test_support::spawn_test_hub().await;
        let client = ingest_forward::http_client().unwrap();
        let receiver = MzpMeta::capture();
        let mut buf = Vec::new();
        let mut backoff = Duration::from_millis(100);
        flush_grouped(&client, &hub_url, &receiver, &mut buf, None, &mut backoff).await;
        assert!(buf.is_empty());
    }

    #[tokio::test]
    async fn flush_grouped_groups_by_service() {
        let (hub_url, store) = crate::test_support::spawn_test_hub().await;
        let client = ingest_forward::http_client().unwrap();
        let receiver = MzpMeta::capture();
        let mut buf = vec![
            IngestItem {
                service: "a".into(),
                line: json!({"msg":"a1"}).to_string(),
            },
            IngestItem {
                service: "b".into(),
                line: json!({"msg":"b1"}).to_string(),
            },
            IngestItem {
                service: "a".into(),
                line: json!({"msg":"a2"}).to_string(),
            },
        ];
        let mut backoff = Duration::from_millis(100);
        flush_grouped(
            &client,
            &format!("{hub_url}/api/ingest/batch"),
            &receiver,
            &mut buf,
            None,
            &mut backoff,
        )
        .await;
        assert!(buf.is_empty());
        tokio::time::sleep(Duration::from_millis(50)).await;
        let entries = store.snapshot_entries().await;
        assert_eq!(entries.len(), 3);
    }

    #[tokio::test]
    async fn flush_grouped_with_service_override() {
        let (hub_url, store) = crate::test_support::spawn_test_hub().await;
        let client = ingest_forward::http_client().unwrap();
        let receiver = MzpMeta::capture();
        let mut buf = vec![IngestItem {
            service: "original".into(),
            line: json!({"msg":"test"}).to_string(),
        }];
        let mut backoff = Duration::from_millis(100);
        flush_grouped(
            &client,
            &format!("{hub_url}/api/ingest/batch"),
            &receiver,
            &mut buf,
            Some("overridden"),
            &mut backoff,
        )
        .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let entries = store.snapshot_entries().await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].service, "overridden");
    }

    #[tokio::test]
    async fn flush_grouped_splits_large_batch() {
        let (hub_url, store) = crate::test_support::spawn_test_hub().await;
        let client = ingest_forward::http_client().unwrap();
        let receiver = MzpMeta::capture();
        let mut buf = Vec::new();
        for i in 0..200 {
            buf.push(IngestItem {
                service: "test".into(),
                line: json!({"msg": format!("line{}", i)}).to_string(),
            });
        }
        let mut backoff = Duration::from_millis(100);
        flush_grouped(
            &client,
            &format!("{hub_url}/api/ingest/batch"),
            &receiver,
            &mut buf,
            None,
            &mut backoff,
        )
        .await;
        assert!(buf.is_empty());
        tokio::time::sleep(Duration::from_millis(100)).await;
        let entries = store.snapshot_entries().await;
        assert!(entries.len() >= 100);
    }

    #[test]
    fn browser_attach_opts_default() {
        let opts = BrowserAttachOpts::default();
        assert_eq!(opts.host, crate::hub::DEFAULT_HOST);
        assert_eq!(opts.port, crate::hub::DEFAULT_PORT);
        assert_eq!(opts.cdp_port, DEFAULT_CDP_PORT);
        assert!(!opts.launch);
        assert!(!opts.all_network);
    }

    #[tokio::test]
    async fn run_ingest_forwarder_flushes_and_exits() {
        let (hub_url, store) = crate::test_support::spawn_test_hub().await;
        let url = url::Url::parse(&hub_url).unwrap();
        let host = url.host_str().unwrap().to_string();
        let port = url.port().unwrap();
        let (tx, rx) = mpsc::channel::<IngestItem>(8);
        let forwarder = tokio::spawn(async move {
            run_ingest_forwarder(rx, host, port, Some("fwd".into())).await;
        });
        tx.send(IngestItem {
            service: "ignored".into(),
            line: json!({"msg":"from-forwarder"}).to_string(),
        })
        .await
        .unwrap();
        drop(tx);
        forwarder.await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let entries = store.snapshot_entries().await;
        assert!(entries.iter().any(|e| e.service == "fwd"));
    }

    #[tokio::test]
    async fn bridge_exits_on_interrupt() {
        let opts = BrowserAttachOpts {
            host: "127.0.0.1".into(),
            port: 1,
            cdp_url: Some("ws://127.0.0.1:1/devtools/browser/none".into()),
            ..Default::default()
        };
        // Interrupt immediately — no live hub/CDP required.
        tokio::time::timeout(
            Duration::from_secs(5),
            run_browser_attach_bridge(opts, "ws://127.0.0.1:1/x".into(), async { Ok(()) }),
        )
        .await
        .expect("bridge should exit promptly on interrupt")
        .unwrap();
    }

    #[tokio::test]
    async fn run_browser_attach_until_with_mocked_hub_probe() {
        let opts = BrowserAttachOpts {
            host: "127.0.0.1".into(),
            port: 1,
            cdp_url: Some("ws://127.0.0.1:1/devtools/browser/none".into()),
            launch: false,
            ..Default::default()
        };
        let initial_ws = resolve_cdp_ws_url(&opts).await.unwrap();
        tokio::time::timeout(
            Duration::from_secs(5),
            run_browser_attach_bridge(opts, initial_ws, async { Ok(()) }),
        )
        .await
        .expect("bridge should exit promptly")
        .unwrap();
    }
}
