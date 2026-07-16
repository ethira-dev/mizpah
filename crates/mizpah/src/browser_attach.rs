//! Chrome/Edge CDP bridge: console + network → hub ingest.

use crate::mzp_meta::MzpMeta;
use crate::shell_attach;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, info, warn};
use url::Url;

const DEFAULT_CDP_PORT: u16 = 9222;
const BODY_MAX_BYTES: usize = 256 * 1024;
const QUEUE_CAPACITY: usize = 4096;
const BATCH_MAX: usize = 128;
const BATCH_FLUSH: Duration = Duration::from_millis(50);
const HTTP_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_BACKOFF: Duration = Duration::from_secs(5);
const CDP_CALL_TIMEOUT: Duration = Duration::from_secs(5);
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
            host: shell_attach::DEFAULT_HOST.to_string(),
            port: shell_attach::DEFAULT_PORT,
            cdp_port: DEFAULT_CDP_PORT,
            cdp_url: None,
            launch: false,
            all_network: false,
        }
    }
}

#[derive(Debug, Clone)]
struct IngestItem {
    service: String,
    line: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchBody<'a> {
    service: &'a str,
    mzp: &'a MzpMeta,
    lines: &'a [String],
}

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// Run browser attach until Ctrl-C.
pub async fn run_browser_attach(opts: BrowserAttachOpts) -> Result<(), String> {
    shell_attach::ensure_hub(&opts.host, opts.port, None).await?;

    if opts.launch {
        let _child = launch_browser(opts.cdp_port)?;
        wait_for_cdp(opts.cdp_port).await?;
    }

    let initial_ws = resolve_cdp_ws_url(&opts).await?;
    info!(
        %initial_ws,
        hub = %shell_attach::hub_url(&opts.host, opts.port),
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
            match run_cdp_session(&ws_url, tx.clone(), all_network).await {
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

async fn resolve_cdp_ws_url_for_reconnect(
    cdp_port: u16,
    cdp_url: Option<&str>,
) -> Result<String, String> {
    if let Some(url) = cdp_url {
        let t = url.trim();
        if !t.is_empty() {
            return Ok(t.to_string());
        }
    }
    fetch_browser_ws_url(cdp_port).await
}

async fn run_cdp_session(
    ws_url: &str,
    tx: mpsc::Sender<IngestItem>,
    all_network: bool,
) -> Result<(), String> {
    let (ws, _) = connect_async(ws_url)
        .await
        .map_err(|e| format!("CDP websocket connect failed: {e}"))?;
    let (write, mut read) = ws.split();
    let write = Arc::new(Mutex::new(write));
    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
    let next_id = Arc::new(AtomicU64::new(1));

    let (event_tx, mut event_rx) = mpsc::channel::<Value>(1024);

    // Reader task: route command replies vs events.
    let pending_r = pending.clone();
    let write_r = write.clone();
    let reader = tokio::spawn(async move {
        while let Some(msg) = read.next().await {
            let msg = match msg {
                Ok(m) => m,
                Err(err) => {
                    warn!(error = %err, "browser attach: CDP read error");
                    break;
                }
            };
            match msg {
                Message::Text(t) => {
                    let value: Value = match serde_json::from_str(&t) {
                        Ok(v) => v,
                        Err(err) => {
                            debug!(error = %err, "browser attach: bad CDP JSON");
                            continue;
                        }
                    };
                    if let Some(id) = value.get("id").and_then(|v| v.as_u64()) {
                        let mut map = pending_r.lock().await;
                        if let Some(tx_resp) = map.remove(&id) {
                            if let Some(err) = value.get("error") {
                                let msg = err
                                    .get("message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("CDP error")
                                    .to_string();
                                let _ = tx_resp.send(Err(msg));
                            } else {
                                let result = value.get("result").cloned().unwrap_or(Value::Null);
                                let _ = tx_resp.send(Ok(result));
                            }
                        }
                    } else if value.get("method").is_some() && event_tx.send(value).await.is_err() {
                        break;
                    }
                }
                Message::Ping(p) => {
                    let mut w = write_r.lock().await;
                    let _ = w.send(Message::Pong(p)).await;
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    cdp_call(
        &write,
        &pending,
        &next_id,
        "Target.setDiscoverTargets",
        json!({"discover": true}),
        None,
    )
    .await?;
    cdp_call(
        &write,
        &pending,
        &next_id,
        "Target.setAutoAttach",
        json!({
            "autoAttach": true,
            "flatten": true,
            "waitForDebuggerOnStart": true
        }),
        None,
    )
    .await?;

    let mut sessions: HashMap<String, PageSession> = HashMap::new();
    let mut network: HashMap<String, PendingNetwork> = HashMap::new();

    while let Some(value) = event_rx.recv().await {
        let method = value.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = value.get("params").cloned().unwrap_or(Value::Null);
        let session_id = value
            .get("sessionId")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());

        match method {
            "Target.attachedToTarget" => {
                let sid = params
                    .get("sessionId")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let target_info = params.get("targetInfo").cloned().unwrap_or(Value::Null);
                let target_type = target_info
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                let url = target_info
                    .get("url")
                    .and_then(|u| u.as_str())
                    .unwrap_or("")
                    .to_string();

                if sid.is_empty() {
                    continue;
                }

                if target_type != "page" {
                    let _ = cdp_call(
                        &write,
                        &pending,
                        &next_id,
                        "Runtime.runIfWaitingForDebugger",
                        json!({}),
                        Some(&sid),
                    )
                    .await;
                    let _ = cdp_call(
                        &write,
                        &pending,
                        &next_id,
                        "Target.detachFromTarget",
                        json!({"sessionId": sid}),
                        None,
                    )
                    .await;
                    continue;
                }

                sessions.insert(
                    sid.clone(),
                    PageSession {
                        page_url: url.clone(),
                        host: service_from_page_url(&url),
                    },
                );

                for (m, p) in [
                    ("Page.enable", json!({})),
                    ("Runtime.enable", json!({})),
                    ("Log.enable", json!({})),
                    (
                        "Network.enable",
                        json!({"maxPostDataSize": BODY_MAX_BYTES as i64}),
                    ),
                ] {
                    if let Err(err) = cdp_call(&write, &pending, &next_id, m, p, Some(&sid)).await {
                        warn!(error = %err, session = %sid, method = m, "enable failed");
                    }
                }
                let _ = cdp_call(
                    &write,
                    &pending,
                    &next_id,
                    "Runtime.runIfWaitingForDebugger",
                    json!({}),
                    Some(&sid),
                )
                .await;
                info!(session = %sid, page = %url, "browser attach: page session ready");
            }
            "Target.detachedFromTarget" => {
                if let Some(sid) = params.get("sessionId").and_then(|s| s.as_str()) {
                    sessions.remove(sid);
                    network.retain(|_, p| p.session_id != sid);
                }
            }
            "Page.frameNavigated" => {
                let Some(sid) = session_id.as_deref() else {
                    continue;
                };
                let frame = params.get("frame").cloned().unwrap_or(Value::Null);
                if frame.get("parentId").and_then(|p| p.as_str()).is_some() {
                    continue;
                }
                let url = frame
                    .get("url")
                    .and_then(|u| u.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(sess) = sessions.get_mut(sid) {
                    sess.page_url = url.clone();
                    sess.host = service_from_page_url(&url);
                }
            }
            "Runtime.consoleAPICalled" => {
                let Some(sid) = session_id.as_deref() else {
                    continue;
                };
                let (page_url, host) = session_page(&sessions, sid);
                if let Some(item) = map_console_api(&params, &page_url, &host) {
                    enqueue(&tx, item);
                }
            }
            "Log.entryAdded" => {
                let Some(sid) = session_id.as_deref() else {
                    continue;
                };
                let (page_url, host) = session_page(&sessions, sid);
                if let Some(item) = map_log_entry(&params, &page_url, &host) {
                    enqueue(&tx, item);
                }
            }
            "Runtime.exceptionThrown" => {
                let Some(sid) = session_id.as_deref() else {
                    continue;
                };
                let (page_url, host) = session_page(&sessions, sid);
                if let Some(item) = map_exception(&params, &page_url, &host) {
                    enqueue(&tx, item);
                }
            }
            "Network.requestWillBeSent" => {
                let Some(sid) = session_id.as_deref() else {
                    continue;
                };
                let request_id = params
                    .get("requestId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if request_id.is_empty() {
                    continue;
                }
                let resource_type = params
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Other")
                    .to_string();
                if !should_emit_network(&resource_type, all_network) {
                    continue;
                }
                let request = params.get("request").cloned().unwrap_or(Value::Null);
                let url = request
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let method_http = request
                    .get("method")
                    .and_then(|v| v.as_str())
                    .unwrap_or("GET")
                    .to_string();
                let headers = request
                    .get("headers")
                    .cloned()
                    .unwrap_or(Value::Object(Map::new()));
                let request_body = extract_request_body(&request);
                let timestamp = params
                    .get("timestamp")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                network.insert(
                    request_id.clone(),
                    PendingNetwork {
                        session_id: sid.to_string(),
                        request_id,
                        method: method_http,
                        url,
                        resource_type,
                        request_headers: headers,
                        request_body,
                        status: None,
                        mime_type: None,
                        response_headers: None,
                        started_at: timestamp,
                    },
                );
            }
            "Network.responseReceived" => {
                let request_id = params
                    .get("requestId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let Some(pending_net) = network.get_mut(request_id) else {
                    continue;
                };
                let response = params.get("response").cloned().unwrap_or(Value::Null);
                pending_net.status = response.get("status").and_then(|v| v.as_u64());
                pending_net.mime_type = response
                    .get("mimeType")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                pending_net.response_headers = Some(
                    response
                        .get("headers")
                        .cloned()
                        .unwrap_or(Value::Object(Map::new())),
                );
            }
            "Network.loadingFinished" => {
                let request_id = params
                    .get("requestId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let Some(pending_net) = network.remove(&request_id) else {
                    continue;
                };
                let finished_at = params
                    .get("timestamp")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let duration_ms = if pending_net.started_at > 0.0 && finished_at > 0.0 {
                    Some((finished_at - pending_net.started_at) * 1000.0)
                } else {
                    None
                };

                let mut response_body: Option<EncodedBody> = None;
                if should_fetch_body(&pending_net.resource_type) && !skip_body_url(&pending_net.url)
                {
                    match cdp_call(
                        &write,
                        &pending,
                        &next_id,
                        "Network.getResponseBody",
                        json!({"requestId": request_id}),
                        Some(&pending_net.session_id),
                    )
                    .await
                    {
                        Ok(body_val) => response_body = decode_cdp_body(&body_val),
                        Err(err) => {
                            debug!(error = %err, %request_id, "getResponseBody failed");
                        }
                    }
                }

                let (page_url, host) = session_page(&sessions, &pending_net.session_id);
                if let Some(item) =
                    map_network_finished(&pending_net, response_body, duration_ms, &page_url, &host)
                {
                    enqueue(&tx, item);
                }
            }
            "Network.loadingFailed" => {
                let request_id = params
                    .get("requestId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let Some(pending_net) = network.remove(&request_id) else {
                    continue;
                };
                let error_text = params
                    .get("errorText")
                    .and_then(|v| v.as_str())
                    .unwrap_or("failed")
                    .to_string();
                let canceled = params
                    .get("canceled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let (page_url, host) = session_page(&sessions, &pending_net.session_id);
                if let Some(item) =
                    map_network_failed(&pending_net, &error_text, canceled, &page_url, &host)
                {
                    enqueue(&tx, item);
                }
            }
            _ => {}
        }
    }

    reader.abort();
    Ok(())
}

async fn cdp_call<S>(
    write: &Arc<Mutex<S>>,
    pending: &PendingMap,
    next_id: &Arc<AtomicU64>,
    method: &str,
    params: Value,
    session_id: Option<&str>,
) -> Result<Value, String>
where
    S: SinkExt<Message> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::fmt::Display,
{
    let id = next_id.fetch_add(1, Ordering::Relaxed);
    let (resp_tx, resp_rx) = oneshot::channel();
    pending.lock().await.insert(id, resp_tx);

    let mut msg = json!({"id": id, "method": method, "params": params});
    if let Some(sid) = session_id {
        msg.as_object_mut()
            .unwrap()
            .insert("sessionId".into(), Value::String(sid.to_string()));
    }

    {
        let mut w = write.lock().await;
        w.send(Message::Text(msg.to_string().into()))
            .await
            .map_err(|e| format!("CDP send failed: {e}"))?;
    }

    match tokio::time::timeout(CDP_CALL_TIMEOUT, resp_rx).await {
        Ok(Ok(Ok(v))) => Ok(v),
        Ok(Ok(Err(e))) => Err(e),
        Ok(Err(_)) => Err("CDP response channel closed".into()),
        Err(_) => {
            pending.lock().await.remove(&id);
            Err(format!("CDP timeout waiting for {method}"))
        }
    }
}

#[derive(Debug, Clone)]
struct PageSession {
    page_url: String,
    host: String,
}

#[derive(Debug, Clone)]
struct PendingNetwork {
    session_id: String,
    request_id: String,
    method: String,
    url: String,
    resource_type: String,
    request_headers: Value,
    request_body: Option<EncodedBody>,
    status: Option<u64>,
    mime_type: Option<String>,
    response_headers: Option<Value>,
    started_at: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedBody {
    pub data: String,
    pub encoding: &'static str,
    pub truncated: bool,
}

fn session_page(sessions: &HashMap<String, PageSession>, sid: &str) -> (String, String) {
    sessions
        .get(sid)
        .map(|s| (s.page_url.clone(), s.host.clone()))
        .unwrap_or_else(|| (String::new(), "browser".into()))
}

fn enqueue(tx: &mpsc::Sender<IngestItem>, item: IngestItem) {
    match tx.try_send(item) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            debug!("browser attach: dropped event (queue full)");
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {}
    }
}

async fn run_ingest_forwarder(
    mut rx: mpsc::Receiver<IngestItem>,
    hub_host: String,
    hub_port: u16,
    service_override: Option<String>,
) {
    let client = match reqwest::Client::builder().timeout(HTTP_TIMEOUT).build() {
        Ok(c) => c,
        Err(err) => {
            warn!(error = %err, "browser attach: http client failed");
            while rx.recv().await.is_some() {}
            return;
        }
    };
    let receiver = MzpMeta::capture();
    let url = format!(
        "{}/api/ingest/batch",
        shell_attach::hub_url(&hub_host, hub_port)
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
            match post_batch(client, url, &service, receiver, &chunk).await {
                Ok(()) => {
                    *backoff = Duration::from_millis(100);
                }
                Err(err) => {
                    warn!(error = %err, %service, "browser attach: batch ingest failed");
                    tokio::time::sleep(*backoff).await;
                    *backoff = (*backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    }
}

fn resolve_service(override_svc: Option<&str>, host: &str) -> String {
    if let Some(s) = override_svc {
        let t = s.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    let t = host.trim();
    if t.is_empty() {
        "browser".into()
    } else {
        t.to_string()
    }
}

async fn post_batch(
    client: &reqwest::Client,
    url: &str,
    service: &str,
    mzp: &MzpMeta,
    lines: &[String],
) -> Result<(), String> {
    let body = BatchBody {
        service,
        mzp,
        lines,
    };
    let resp = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status() == reqwest::StatusCode::CONFLICT {
        return Err("service disconnected".into());
    }
    if !resp.status().is_success() {
        return Err(format!("status {}", resp.status()));
    }
    Ok(())
}

// --- CDP endpoint / launch -------------------------------------------------

async fn resolve_cdp_ws_url(opts: &BrowserAttachOpts) -> Result<String, String> {
    if let Some(ref url) = opts.cdp_url {
        let t = url.trim();
        if t.is_empty() {
            return Err("--cdp-url must not be empty".into());
        }
        return Ok(t.to_string());
    }
    fetch_browser_ws_url(opts.cdp_port).await
}

async fn fetch_browser_ws_url(cdp_port: u16) -> Result<String, String> {
    let version_url = format!("http://127.0.0.1:{cdp_port}/json/version");
    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(&version_url).send().await.map_err(|e| {
        format!(
            "cannot reach Chrome DevTools at {version_url}: {e}\n\
             Start Chrome with --remote-debugging-port={cdp_port}, or use `mzp attach browser --launch`"
        )
    })?;
    if !resp.status().is_success() {
        return Err(format!(
            "Chrome DevTools at {version_url} returned {}",
            resp.status()
        ));
    }
    let body: Value = resp.json().await.map_err(|e| e.to_string())?;
    body.get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "Chrome /json/version missing webSocketDebuggerUrl".into())
}

async fn wait_for_cdp(cdp_port: u16) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut last_err = String::new();
    while tokio::time::Instant::now() < deadline {
        match fetch_browser_ws_url(cdp_port).await {
            Ok(_) => return Ok(()),
            Err(e) => last_err = e,
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    Err(format!(
        "timed out waiting for CDP on :{cdp_port}: {last_err}"
    ))
}

fn launch_browser(cdp_port: u16) -> Result<std::process::Child, String> {
    let binary = find_browser_binary().ok_or_else(|| {
        "could not find Google Chrome or Microsoft Edge; install one or pass --cdp-url".to_string()
    })?;
    let profile = chrome_profile_dir()?;
    std::fs::create_dir_all(&profile)
        .map_err(|e| format!("failed to create chrome profile dir: {e}"))?;

    let mut cmd = std::process::Command::new(&binary);
    cmd.arg(format!("--remote-debugging-port={cdp_port}"))
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: detach from controlling terminal so Ctrl-C on mzp doesn't kill Chrome.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to launch {}: {e}", binary.display()))?;
    info!(
        binary = %binary.display(),
        profile = %profile.display(),
        cdp_port,
        "browser attach: launched browser (dedicated profile)"
    );
    Ok(child)
}

fn chrome_profile_dir() -> Result<PathBuf, String> {
    let dir = shell_attach::config_dir().map_err(|e| e.to_string())?;
    Ok(dir.join("chrome-profile"))
}

fn find_browser_binary() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let candidates = [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
        ];
        candidates
            .into_iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
    }
    #[cfg(target_os = "linux")]
    {
        for name in [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
            "microsoft-edge",
        ] {
            if let Some(p) = which_bin(name) {
                return Some(p);
            }
        }
        return None;
    }
    #[cfg(target_os = "windows")]
    {
        let mut candidates = Vec::new();
        if let Ok(pf) = std::env::var("PROGRAMFILES") {
            candidates.push(PathBuf::from(&pf).join("Google\\Chrome\\Application\\chrome.exe"));
            candidates.push(PathBuf::from(&pf).join("Microsoft\\Edge\\Application\\msedge.exe"));
        }
        if let Ok(pf86) = std::env::var("PROGRAMFILES(X86)") {
            candidates.push(PathBuf::from(&pf86).join("Google\\Chrome\\Application\\chrome.exe"));
            candidates.push(PathBuf::from(&pf86).join("Microsoft\\Edge\\Application\\msedge.exe"));
        }
        return candidates.into_iter().find(|p| p.is_file());
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
fn which_bin(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// --- Event mappers (unit-tested) -------------------------------------------

/// Derive hub service name from a page URL (`location.host` semantics).
pub fn service_from_page_url(page_url: &str) -> String {
    let trimmed = page_url.trim();
    if trimmed.is_empty() || trimmed == "about:blank" {
        return "browser".into();
    }
    let Ok(url) = Url::parse(trimmed) else {
        return "browser".into();
    };
    match url.scheme() {
        "chrome" | "chrome-extension" | "devtools" | "chrome-search" | "chrome-untrusted" => {
            return "chrome-internal".into();
        }
        "file" => return "file".into(),
        _ => {}
    }
    let host = match url.host_str() {
        Some(h) if !h.is_empty() => h,
        _ => return "browser".into(),
    };
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    }
}

pub fn should_emit_network(resource_type: &str, all_network: bool) -> bool {
    if all_network {
        return true;
    }
    matches!(resource_type, "Document" | "XHR" | "Fetch" | "WebSocket")
}

pub fn should_fetch_body(resource_type: &str) -> bool {
    matches!(resource_type, "Document" | "XHR" | "Fetch")
}

pub fn skip_body_url(url: &str) -> bool {
    url.starts_with("data:") || url.starts_with("blob:")
}

/// Truncate and encode a body as utf8 or base64.
pub fn encode_body_bytes(bytes: &[u8]) -> EncodedBody {
    let truncated = bytes.len() > BODY_MAX_BYTES;
    let slice = if truncated {
        &bytes[..BODY_MAX_BYTES]
    } else {
        bytes
    };
    if let Ok(s) = std::str::from_utf8(slice) {
        if !s.contains('\0') {
            return EncodedBody {
                data: s.to_string(),
                encoding: "utf8",
                truncated,
            };
        }
    }
    EncodedBody {
        data: base64::engine::general_purpose::STANDARD.encode(slice),
        encoding: "base64",
        truncated,
    }
}

pub fn encode_body_str(s: &str) -> EncodedBody {
    encode_body_bytes(s.as_bytes())
}

fn decode_cdp_body(body_val: &Value) -> Option<EncodedBody> {
    let body = body_val.get("body")?.as_str()?;
    let base64_encoded = body_val
        .get("base64Encoded")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if base64_encoded {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(body)
            .ok()?;
        Some(encode_body_bytes(&bytes))
    } else {
        Some(encode_body_str(body))
    }
}

fn extract_request_body(request: &Value) -> Option<EncodedBody> {
    if let Some(post) = request.get("postData").and_then(|v| v.as_str()) {
        return Some(encode_body_str(post));
    }
    if let Some(entries) = request.get("postDataEntries").and_then(|v| v.as_array()) {
        let mut combined = String::new();
        for entry in entries {
            if let Some(bytes) = entry.get("bytes").and_then(|v| v.as_str()) {
                if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(bytes) {
                    if let Ok(s) = String::from_utf8(decoded) {
                        combined.push_str(&s);
                        continue;
                    }
                }
                combined.push_str(bytes);
            }
        }
        if !combined.is_empty() {
            return Some(encode_body_str(&combined));
        }
    }
    None
}

fn remote_object_to_json(obj: &Value) -> Value {
    if let Some(v) = obj.get("value") {
        return v.clone();
    }
    if let Some(unserializable) = obj.get("unserializableValue") {
        return unserializable.clone();
    }
    if let Some(preview) = obj.get("preview") {
        if let Some(props) = preview.get("properties").and_then(|p| p.as_array()) {
            let mut map = Map::new();
            for p in props {
                let name = p
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let val = p
                    .get("value")
                    .cloned()
                    .or_else(|| {
                        p.get("valuePreview")
                            .and_then(|v| v.get("description"))
                            .cloned()
                    })
                    .unwrap_or(Value::Null);
                if !name.is_empty() {
                    map.insert(name, val);
                }
            }
            if !map.is_empty() {
                return Value::Object(map);
            }
        }
    }
    let type_name = obj
        .get("className")
        .or_else(|| obj.get("type"))
        .and_then(|t| t.as_str())
        .unwrap_or("object");
    let description = obj
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or(type_name);
    json!({
        "_type": type_name,
        "description": description,
    })
}

fn console_level(cdp_type: &str) -> &'static str {
    match cdp_type {
        "error" | "assert" => "error",
        "warning" => "warn",
        "info" => "info",
        "debug" | "verbose" => "debug",
        "trace" => "trace",
        _ => "log",
    }
}

fn map_console_api(params: &Value, page_url: &str, host: &str) -> Option<IngestItem> {
    let level = console_level(params.get("type").and_then(|t| t.as_str()).unwrap_or("log"));
    let args_raw = params.get("args").and_then(|a| a.as_array());
    let args: Vec<Value> = args_raw
        .map(|a| a.iter().map(remote_object_to_json).collect())
        .unwrap_or_default();
    let msg = format_console_msg(&args);
    let ts = params.get("timestamp").cloned().unwrap_or(Value::Null);
    let payload = json!({
        "source": "browser",
        "kind": "console",
        "browser": "chrome",
        "level": level,
        "msg": msg,
        "args": args,
        "pageUrl": page_url,
        "host": host,
        "hostname": host_only(host),
        "ts": ts,
    });
    Some(IngestItem {
        service: host.to_string(),
        line: payload.to_string(),
    })
}

fn format_console_msg(args: &[Value]) -> String {
    if args.is_empty() {
        return String::new();
    }
    args.iter()
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn host_only(host: &str) -> String {
    host.split(':').next().unwrap_or(host).to_string()
}

fn map_log_entry(params: &Value, page_url: &str, host: &str) -> Option<IngestItem> {
    let entry = params.get("entry")?;
    let level = match entry
        .get("level")
        .and_then(|l| l.as_str())
        .unwrap_or("info")
    {
        "error" => "error",
        "warning" => "warn",
        "verbose" => "debug",
        _ => "info",
    };
    let msg = entry
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let url = entry
        .get("url")
        .and_then(|u| u.as_str())
        .unwrap_or(page_url);
    let payload = json!({
        "source": "browser",
        "kind": "console",
        "browser": "chrome",
        "level": level,
        "msg": msg,
        "args": [msg.clone()],
        "pageUrl": url,
        "host": host,
        "hostname": host_only(host),
        "ts": entry.get("timestamp").cloned().unwrap_or(Value::Null),
    });
    Some(IngestItem {
        service: host.to_string(),
        line: payload.to_string(),
    })
}

fn map_exception(params: &Value, page_url: &str, host: &str) -> Option<IngestItem> {
    let details = params.get("exceptionDetails")?;
    let msg = details
        .get("text")
        .and_then(|t| t.as_str())
        .or_else(|| {
            details
                .get("exception")
                .and_then(|e| e.get("description"))
                .and_then(|d| d.as_str())
        })
        .unwrap_or("uncaught exception")
        .to_string();
    let payload = json!({
        "source": "browser",
        "kind": "console",
        "browser": "chrome",
        "level": "error",
        "msg": msg,
        "args": [msg.clone()],
        "pageUrl": page_url,
        "host": host,
        "hostname": host_only(host),
        "exception": details,
        "ts": params.get("timestamp").cloned().unwrap_or(Value::Null),
    });
    Some(IngestItem {
        service: host.to_string(),
        line: payload.to_string(),
    })
}

fn map_network_finished(
    pending: &PendingNetwork,
    response_body: Option<EncodedBody>,
    duration_ms: Option<f64>,
    page_url: &str,
    host: &str,
) -> Option<IngestItem> {
    let mut payload = json!({
        "source": "browser",
        "kind": "network",
        "browser": "chrome",
        "requestId": pending.request_id,
        "method": pending.method,
        "url": pending.url,
        "status": pending.status,
        "mimeType": pending.mime_type,
        "resourceType": pending.resource_type,
        "durationMs": duration_ms,
        "requestHeaders": pending.request_headers,
        "responseHeaders": pending.response_headers.clone().unwrap_or(Value::Object(Map::new())),
        "pageUrl": page_url,
        "host": host,
        "hostname": host_only(host),
    });
    let obj = payload.as_object_mut()?;
    if let Some(rb) = &pending.request_body {
        obj.insert("requestBody".into(), Value::String(rb.data.clone()));
        obj.insert(
            "requestBodyEncoding".into(),
            Value::String(rb.encoding.into()),
        );
        obj.insert("requestBodyTruncated".into(), Value::Bool(rb.truncated));
    }
    if let Some(rb) = response_body {
        obj.insert("responseBody".into(), Value::String(rb.data));
        obj.insert(
            "responseBodyEncoding".into(),
            Value::String(rb.encoding.into()),
        );
        obj.insert("responseBodyTruncated".into(), Value::Bool(rb.truncated));
    }
    Some(IngestItem {
        service: host.to_string(),
        line: payload.to_string(),
    })
}

fn map_network_failed(
    pending: &PendingNetwork,
    error_text: &str,
    canceled: bool,
    page_url: &str,
    host: &str,
) -> Option<IngestItem> {
    let mut payload = json!({
        "source": "browser",
        "kind": "network",
        "browser": "chrome",
        "requestId": pending.request_id,
        "method": pending.method,
        "url": pending.url,
        "status": pending.status,
        "mimeType": pending.mime_type,
        "resourceType": pending.resource_type,
        "requestHeaders": pending.request_headers,
        "responseHeaders": pending.response_headers.clone().unwrap_or(Value::Object(Map::new())),
        "errorText": error_text,
        "canceled": canceled,
        "pageUrl": page_url,
        "host": host,
        "hostname": host_only(host),
    });
    let obj = payload.as_object_mut()?;
    if let Some(rb) = &pending.request_body {
        obj.insert("requestBody".into(), Value::String(rb.data.clone()));
        obj.insert(
            "requestBodyEncoding".into(),
            Value::String(rb.encoding.into()),
        );
        obj.insert("requestBodyTruncated".into(), Value::Bool(rb.truncated));
    }
    Some(IngestItem {
        service: host.to_string(),
        line: payload.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn service_from_localhost_with_port() {
        assert_eq!(
            service_from_page_url("http://localhost:5173/dashboard"),
            "localhost:5173"
        );
    }

    #[test]
    fn service_from_https_default_port() {
        assert_eq!(
            service_from_page_url("https://app.example.com/path"),
            "app.example.com"
        );
    }

    #[test]
    fn service_fallbacks() {
        assert_eq!(service_from_page_url("about:blank"), "browser");
        assert_eq!(service_from_page_url(""), "browser");
        assert_eq!(
            service_from_page_url("chrome://settings"),
            "chrome-internal"
        );
        assert_eq!(service_from_page_url("file:///tmp/x.html"), "file");
    }

    #[test]
    fn network_filter_defaults() {
        assert!(should_emit_network("Fetch", false));
        assert!(should_emit_network("XHR", false));
        assert!(should_emit_network("Document", false));
        assert!(should_emit_network("WebSocket", false));
        assert!(!should_emit_network("Image", false));
        assert!(should_emit_network("Image", true));
        assert!(should_fetch_body("Fetch"));
        assert!(!should_fetch_body("Image"));
        assert!(skip_body_url("data:text/plain,hi"));
        assert!(skip_body_url("blob:https://x/1"));
        assert!(!skip_body_url("https://api.example/v1"));
    }

    #[test]
    fn encode_body_utf8_and_truncate() {
        let small = encode_body_str("hello");
        assert_eq!(small.encoding, "utf8");
        assert!(!small.truncated);
        assert_eq!(small.data, "hello");

        let big = vec![b'a'; BODY_MAX_BYTES + 10];
        let enc = encode_body_bytes(&big);
        assert!(enc.truncated);
        assert_eq!(enc.data.len(), BODY_MAX_BYTES);
        assert_eq!(enc.encoding, "utf8");
    }

    #[test]
    fn encode_body_base64_for_binary() {
        let bytes = [0u8, 1, 2, 255, 0, 3];
        let enc = encode_body_bytes(&bytes);
        assert_eq!(enc.encoding, "base64");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&enc.data)
            .unwrap();
        assert_eq!(decoded, bytes);
    }

    #[test]
    fn resolve_service_override() {
        assert_eq!(resolve_service(Some("web"), "localhost:5173"), "web");
        assert_eq!(resolve_service(None, "localhost:5173"), "localhost:5173");
        assert_eq!(
            resolve_service(Some("  "), "localhost:5173"),
            "localhost:5173"
        );
    }

    #[test]
    fn map_console_log() {
        let params = json!({
            "type": "log",
            "args": [
                {"type": "string", "value": "hello"},
                {"type": "number", "value": 42}
            ],
            "timestamp": 1.5
        });
        let item = map_console_api(&params, "http://localhost:5173/", "localhost:5173").unwrap();
        assert_eq!(item.service, "localhost:5173");
        let v: Value = serde_json::from_str(&item.line).unwrap();
        assert_eq!(v["kind"], "console");
        assert_eq!(v["level"], "log");
        assert_eq!(v["msg"], "hello 42");
        assert_eq!(v["host"], "localhost:5173");
    }

    #[test]
    fn map_console_warning_to_warn() {
        let params = json!({
            "type": "warning",
            "args": [{"type": "string", "value": "careful"}],
            "timestamp": 1
        });
        let item = map_console_api(&params, "https://a.com/", "a.com").unwrap();
        let v: Value = serde_json::from_str(&item.line).unwrap();
        assert_eq!(v["level"], "warn");
    }

    #[test]
    fn map_network_includes_bodies() {
        let pending = PendingNetwork {
            session_id: "s1".into(),
            request_id: "r1".into(),
            method: "POST".into(),
            url: "https://api.example.com/v1".into(),
            resource_type: "Fetch".into(),
            request_headers: json!({"content-type": "application/json"}),
            request_body: Some(encode_body_str(r#"{"a":1}"#)),
            status: Some(201),
            mime_type: Some("application/json".into()),
            response_headers: Some(json!({"content-type": "application/json"})),
            started_at: 1.0,
        };
        let item = map_network_finished(
            &pending,
            Some(encode_body_str(r#"{"id":1}"#)),
            Some(42.5),
            "https://app.example.com/",
            "app.example.com",
        )
        .unwrap();
        assert_eq!(item.service, "app.example.com");
        let v: Value = serde_json::from_str(&item.line).unwrap();
        assert_eq!(v["kind"], "network");
        assert_eq!(v["status"], 201);
        assert_eq!(v["requestBody"], r#"{"a":1}"#);
        assert_eq!(v["responseBody"], r#"{"id":1}"#);
        assert_eq!(v["durationMs"], 42.5);
        assert_eq!(v["host"], "app.example.com");
    }

    #[test]
    fn group_services_for_batch() {
        let items = vec![
            IngestItem {
                service: "a.com".into(),
                line: r#"{"n":1}"#.into(),
            },
            IngestItem {
                service: "b.com".into(),
                line: r#"{"n":2}"#.into(),
            },
            IngestItem {
                service: "a.com".into(),
                line: r#"{"n":3}"#.into(),
            },
        ];
        let mut groups: HashMap<String, Vec<String>> = HashMap::new();
        for item in items {
            let service = resolve_service(None, &item.service);
            groups.entry(service).or_default().push(item.line);
        }
        assert_eq!(groups.get("a.com").unwrap().len(), 2);
        assert_eq!(groups.get("b.com").unwrap().len(), 1);
    }
}
