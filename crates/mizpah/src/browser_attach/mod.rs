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
    crate::hub::ensure_hub(&opts.host, opts.port, None, false)
        .await
        .map_err(|e| e.to_string())?;

    if opts.launch {
        let _child = launch_browser(opts.cdp_port)?;
        wait_for_cdp(opts.cdp_port).await?;
    }

    let initial_ws = resolve_cdp_ws_url(&opts).await?;
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

    let bridge = async {
        let mut backoff = Duration::from_millis(200);
        let mut ws_url = initial_ws;
        loop {
            match cdp::run_cdp_session(&ws_url, tx.clone(), all_network).await {
                Ok(()) => warn!("browser attach: CDP session ended; reconnecting"),
                Err(err) => warn!(error = %err, "browser attach: CDP session error; reconnecting"),
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
            match resolve_cdp_ws_url_for_reconnect(cdp_port, cdp_url_override.as_deref()).await {
                Ok(url) => {
                    ws_url = url;
                    backoff = Duration::from_millis(200);
                }
                Err(err) => warn!(error = %err, "browser attach: CDP endpoint unavailable"),
            }
        }
    };

    tokio::select! {
        _ = bridge => {}
        _ = tokio::signal::ctrl_c() => {
            info!("browser attach: interrupted");
        }
    }

    drop(tx);
    let _ = forwarder.await;
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
