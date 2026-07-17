//! CDP websocket session handling.

use super::map::BODY_MAX_BYTES;
use super::map::{
    decode_cdp_body, extract_request_body, map_console_api, map_exception, map_log_entry,
    map_network_failed, map_network_finished, service_from_page_url, should_emit_network,
    should_fetch_body, skip_body_url, IngestItem, PendingNetwork,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, info, warn};

const CDP_CALL_TIMEOUT: Duration = Duration::from_secs(5);

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

#[derive(Debug, Clone)]
struct PageSession {
    page_url: String,
    host: String,
}

pub(crate) async fn run_cdp_session(
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
                        let mut map_lock = pending_r.lock().await;
                        if let Some(tx_resp) = map_lock.remove(&id) {
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

                let mut response_body = None;
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

fn session_page(sessions: &HashMap<String, PageSession>, sid: &str) -> (String, String) {
    sessions.get(sid).map_or_else(
        || (String::new(), "browser".into()),
        |s| (s.page_url.clone(), s.host.clone()),
    )
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
