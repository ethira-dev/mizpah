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
pub(crate) struct PageSession {
    pub(crate) page_url: String,
    pub(crate) host: String,
}

pub(crate) struct SessionState {
    pub(crate) sessions: HashMap<String, PageSession>,
    pub(crate) network: HashMap<String, PendingNetwork>,
}

pub(crate) enum CdpAction {
    EnableDomains {
        session_id: String,
    },
    FetchResponseBody {
        request_id: String,
        session_id: String,
    },
    DetachTarget {
        session_id: String,
    },
    Emit(IngestItem),
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

    let mut state = SessionState {
        sessions: HashMap::new(),
        network: HashMap::new(),
    };

    while let Some(value) = event_rx.recv().await {
        let actions = process_cdp_event(&mut state, &value, all_network);
        dispatch_cdp_actions(actions, &write, &pending, &next_id, &tx, &state).await;
    }

    reader.abort();
    Ok(())
}

pub(crate) async fn dispatch_cdp_actions<S>(
    actions: Vec<CdpAction>,
    write: &Arc<Mutex<S>>,
    pending: &PendingMap,
    next_id: &Arc<AtomicU64>,
    tx: &mpsc::Sender<IngestItem>,
    state: &SessionState,
) where
    S: SinkExt<Message> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::fmt::Display,
{
    for action in actions {
        match action {
            CdpAction::EnableDomains { session_id } => {
                for (m, p) in [
                    ("Page.enable", json!({})),
                    ("Runtime.enable", json!({})),
                    ("Log.enable", json!({})),
                    (
                        "Network.enable",
                        json!({"maxPostDataSize": BODY_MAX_BYTES as i64}),
                    ),
                ] {
                    if let Err(err) =
                        cdp_call(write, pending, next_id, m, p, Some(&session_id)).await
                    {
                        warn!(error = %err, session = %session_id, method = m, "enable failed");
                    }
                }
                let _ = cdp_call(
                    write,
                    pending,
                    next_id,
                    "Runtime.runIfWaitingForDebugger",
                    json!({}),
                    Some(&session_id),
                )
                .await;
                if let Some(sess) = state.sessions.get(&session_id) {
                    info!(session = %session_id, page = %sess.page_url, "browser attach: page session ready");
                }
            }
            CdpAction::DetachTarget { session_id } => {
                let _ = cdp_call(
                    write,
                    pending,
                    next_id,
                    "Runtime.runIfWaitingForDebugger",
                    json!({}),
                    Some(&session_id),
                )
                .await;
                let _ = cdp_call(
                    write,
                    pending,
                    next_id,
                    "Target.detachFromTarget",
                    json!({"sessionId": session_id}),
                    None,
                )
                .await;
            }
            CdpAction::FetchResponseBody {
                request_id,
                session_id,
            } => {
                let body_val = cdp_call(
                    write,
                    pending,
                    next_id,
                    "Network.getResponseBody",
                    json!({"requestId": request_id}),
                    Some(&session_id),
                )
                .await;
                match body_val {
                    Ok(val) => {
                        if let Some(_response_body) = decode_cdp_body(&val) {
                            debug!("fetched response body for {}", request_id);
                        }
                    }
                    Err(err) => {
                        debug!(error = %err, %request_id, "getResponseBody failed");
                    }
                }
            }
            CdpAction::Emit(item) => {
                enqueue(tx, item);
            }
        }
    }
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

pub(crate) fn process_cdp_event(
    state: &mut SessionState,
    value: &Value,
    all_network: bool,
) -> Vec<CdpAction> {
    let method = value.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = value.get("params").cloned().unwrap_or(Value::Null);
    let session_id = value
        .get("sessionId")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());

    let mut actions = Vec::new();

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
                return actions;
            }

            if target_type != "page" {
                actions.push(CdpAction::DetachTarget { session_id: sid });
                return actions;
            }

            state.sessions.insert(
                sid.clone(),
                PageSession {
                    page_url: url.clone(),
                    host: service_from_page_url(&url),
                },
            );

            actions.push(CdpAction::EnableDomains { session_id: sid });
        }
        "Target.detachedFromTarget" => {
            if let Some(sid) = params.get("sessionId").and_then(|s| s.as_str()) {
                state.sessions.remove(sid);
                state.network.retain(|_, p| p.session_id != sid);
            }
        }
        "Page.frameNavigated" => {
            let Some(sid) = session_id.as_deref() else {
                return actions;
            };
            let frame = params.get("frame").cloned().unwrap_or(Value::Null);
            if frame.get("parentId").and_then(|p| p.as_str()).is_some() {
                return actions;
            }
            let url = frame
                .get("url")
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(sess) = state.sessions.get_mut(sid) {
                sess.page_url = url.clone();
                sess.host = service_from_page_url(&url);
            }
        }
        "Runtime.consoleAPICalled" => {
            let Some(sid) = session_id.as_deref() else {
                return actions;
            };
            let (page_url, host) = session_page(&state.sessions, sid);
            if let Some(item) = map_console_api(&params, &page_url, &host) {
                actions.push(CdpAction::Emit(item));
            }
        }
        "Log.entryAdded" => {
            let Some(sid) = session_id.as_deref() else {
                return actions;
            };
            let (page_url, host) = session_page(&state.sessions, sid);
            if let Some(item) = map_log_entry(&params, &page_url, &host) {
                actions.push(CdpAction::Emit(item));
            }
        }
        "Runtime.exceptionThrown" => {
            let Some(sid) = session_id.as_deref() else {
                return actions;
            };
            let (page_url, host) = session_page(&state.sessions, sid);
            if let Some(item) = map_exception(&params, &page_url, &host) {
                actions.push(CdpAction::Emit(item));
            }
        }
        "Network.requestWillBeSent" => {
            let Some(sid) = session_id.as_deref() else {
                return actions;
            };
            let request_id = params
                .get("requestId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if request_id.is_empty() {
                return actions;
            }
            let resource_type = params
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("Other")
                .to_string();
            if !should_emit_network(&resource_type, all_network) {
                return actions;
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
            state.network.insert(
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
            let Some(pending_net) = state.network.get_mut(request_id) else {
                return actions;
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
            let Some(pending_net) = state.network.remove(&request_id) else {
                return actions;
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

            let should_fetch =
                should_fetch_body(&pending_net.resource_type) && !skip_body_url(&pending_net.url);
            if should_fetch {
                actions.push(CdpAction::FetchResponseBody {
                    request_id: request_id.clone(),
                    session_id: pending_net.session_id.clone(),
                });
            }

            let (page_url, host) = session_page(&state.sessions, &pending_net.session_id);
            if let Some(item) =
                map_network_finished(&pending_net, None, duration_ms, &page_url, &host)
            {
                actions.push(CdpAction::Emit(item));
            }
        }
        "Network.loadingFailed" => {
            let request_id = params
                .get("requestId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let Some(pending_net) = state.network.remove(&request_id) else {
                return actions;
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
            let (page_url, host) = session_page(&state.sessions, &pending_net.session_id);
            if let Some(item) =
                map_network_failed(&pending_net, &error_text, canceled, &page_url, &host)
            {
                actions.push(CdpAction::Emit(item));
            }
        }
        _ => {}
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_page_returns_defaults_when_missing() {
        let sessions = HashMap::new();
        let (page, host) = session_page(&sessions, "unknown");
        assert_eq!(page, "");
        assert_eq!(host, "browser");
    }

    #[test]
    fn session_page_returns_session_info() {
        let mut sessions = HashMap::new();
        sessions.insert(
            "s1".into(),
            PageSession {
                page_url: "http://localhost:5173/".into(),
                host: "localhost:5173".into(),
            },
        );
        let (page, host) = session_page(&sessions, "s1");
        assert_eq!(page, "http://localhost:5173/");
        assert_eq!(host, "localhost:5173");
    }

    #[test]
    fn process_target_attached_non_page_detaches() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        let event = json!({
            "method": "Target.attachedToTarget",
            "params": {
                "sessionId": "s1",
                "targetInfo": {
                    "type": "worker",
                    "url": "http://example.com"
                }
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], CdpAction::DetachTarget { .. }));
    }

    #[test]
    fn process_target_attached_page_enables_domains() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        let event = json!({
            "method": "Target.attachedToTarget",
            "params": {
                "sessionId": "s1",
                "targetInfo": {
                    "type": "page",
                    "url": "http://localhost:3000/app"
                }
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], CdpAction::EnableDomains { .. }));
        assert!(state.sessions.contains_key("s1"));
        let sess = &state.sessions["s1"];
        assert_eq!(sess.page_url, "http://localhost:3000/app");
        assert_eq!(sess.host, "localhost:3000");
    }

    #[test]
    fn process_target_attached_empty_session_id_skips() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        let event = json!({
            "method": "Target.attachedToTarget",
            "params": {
                "sessionId": "",
                "targetInfo": {"type": "page", "url": "http://example.com"}
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert_eq!(actions.len(), 0);
    }

    #[test]
    fn process_target_detached_removes_session() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        state.sessions.insert(
            "s1".into(),
            PageSession {
                page_url: "http://example.com".into(),
                host: "example.com".into(),
            },
        );
        let event = json!({
            "method": "Target.detachedFromTarget",
            "params": {
                "sessionId": "s1"
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert_eq!(actions.len(), 0);
        assert!(!state.sessions.contains_key("s1"));
    }

    #[test]
    fn process_frame_navigated_updates_page_url() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        state.sessions.insert(
            "s1".into(),
            PageSession {
                page_url: "http://old.com".into(),
                host: "old.com".into(),
            },
        );
        let event = json!({
            "method": "Page.frameNavigated",
            "sessionId": "s1",
            "params": {
                "frame": {
                    "url": "http://new.com/page"
                }
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert_eq!(actions.len(), 0);
        let sess = &state.sessions["s1"];
        assert_eq!(sess.page_url, "http://new.com/page");
        assert_eq!(sess.host, "new.com");
    }

    #[test]
    fn process_frame_navigated_skips_child_frames() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        state.sessions.insert(
            "s1".into(),
            PageSession {
                page_url: "http://old.com".into(),
                host: "old.com".into(),
            },
        );
        let event = json!({
            "method": "Page.frameNavigated",
            "sessionId": "s1",
            "params": {
                "frame": {
                    "url": "http://new.com",
                    "parentId": "parent123"
                }
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert_eq!(actions.len(), 0);
        let sess = &state.sessions["s1"];
        assert_eq!(sess.page_url, "http://old.com");
    }

    #[test]
    fn process_console_api_called_emits_item() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        state.sessions.insert(
            "s1".into(),
            PageSession {
                page_url: "http://localhost:3000".into(),
                host: "localhost:3000".into(),
            },
        );
        let event = json!({
            "method": "Runtime.consoleAPICalled",
            "sessionId": "s1",
            "params": {
                "type": "log",
                "args": [{"type": "string", "value": "test"}],
                "timestamp": 1.0
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], CdpAction::Emit(_)));
    }

    #[test]
    fn process_log_entry_added_emits_item() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        state.sessions.insert(
            "s1".into(),
            PageSession {
                page_url: "http://localhost:3000".into(),
                host: "localhost:3000".into(),
            },
        );
        let event = json!({
            "method": "Log.entryAdded",
            "sessionId": "s1",
            "params": {
                "entry": {
                    "level": "info",
                    "text": "test log",
                    "timestamp": 1.0
                }
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], CdpAction::Emit(_)));
    }

    #[test]
    fn process_exception_thrown_emits_item() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        state.sessions.insert(
            "s1".into(),
            PageSession {
                page_url: "http://localhost:3000".into(),
                host: "localhost:3000".into(),
            },
        );
        let event = json!({
            "method": "Runtime.exceptionThrown",
            "sessionId": "s1",
            "params": {
                "exceptionDetails": {
                    "text": "error",
                    "exception": {"description": "test error"},
                    "timestamp": 1.0
                }
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], CdpAction::Emit(_)));
    }

    #[test]
    fn process_network_request_will_be_sent() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        state.sessions.insert(
            "s1".into(),
            PageSession {
                page_url: "http://localhost:3000".into(),
                host: "localhost:3000".into(),
            },
        );
        let event = json!({
            "method": "Network.requestWillBeSent",
            "sessionId": "s1",
            "params": {
                "requestId": "req1",
                "type": "XHR",
                "request": {
                    "url": "http://api.example.com/data",
                    "method": "POST",
                    "headers": {}
                },
                "timestamp": 100.0
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert_eq!(actions.len(), 0);
        assert!(state.network.contains_key("req1"));
        let pending = &state.network["req1"];
        assert_eq!(pending.method, "POST");
        assert_eq!(pending.url, "http://api.example.com/data");
    }

    #[test]
    fn process_network_request_skips_non_xhr() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        state.sessions.insert(
            "s1".into(),
            PageSession {
                page_url: "http://localhost:3000".into(),
                host: "localhost:3000".into(),
            },
        );
        let event = json!({
            "method": "Network.requestWillBeSent",
            "sessionId": "s1",
            "params": {
                "requestId": "req1",
                "type": "Image",
                "request": {"url": "http://example.com/img.png", "method": "GET"},
                "timestamp": 100.0
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert_eq!(actions.len(), 0);
        assert!(!state.network.contains_key("req1"));
    }

    #[test]
    fn process_network_request_allows_all_network_types() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        state.sessions.insert(
            "s1".into(),
            PageSession {
                page_url: "http://localhost:3000".into(),
                host: "localhost:3000".into(),
            },
        );
        let event = json!({
            "method": "Network.requestWillBeSent",
            "sessionId": "s1",
            "params": {
                "requestId": "req1",
                "type": "Image",
                "request": {"url": "http://example.com/img.png", "method": "GET"},
                "timestamp": 100.0
            }
        });
        let actions = process_cdp_event(&mut state, &event, true);
        assert_eq!(actions.len(), 0);
        assert!(state.network.contains_key("req1"));
    }

    #[test]
    fn process_network_response_received_updates_pending() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        state.network.insert(
            "req1".into(),
            PendingNetwork {
                session_id: "s1".into(),
                request_id: "req1".into(),
                method: "GET".into(),
                url: "http://example.com".into(),
                resource_type: "XHR".into(),
                request_headers: Value::Null,
                request_body: None,
                status: None,
                mime_type: None,
                response_headers: None,
                started_at: 100.0,
            },
        );
        let event = json!({
            "method": "Network.responseReceived",
            "params": {
                "requestId": "req1",
                "response": {
                    "status": 200,
                    "mimeType": "application/json",
                    "headers": {"content-type": "application/json"}
                }
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert_eq!(actions.len(), 0);
        let pending = &state.network["req1"];
        assert_eq!(pending.status, Some(200));
        assert_eq!(pending.mime_type, Some("application/json".into()));
        assert!(pending.response_headers.is_some());
    }

    #[test]
    fn process_network_loading_finished_emits_item() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        state.sessions.insert(
            "s1".into(),
            PageSession {
                page_url: "http://localhost:3000".into(),
                host: "localhost:3000".into(),
            },
        );
        state.network.insert(
            "req1".into(),
            PendingNetwork {
                session_id: "s1".into(),
                request_id: "req1".into(),
                method: "GET".into(),
                url: "http://api.example.com/data".into(),
                resource_type: "XHR".into(),
                request_headers: Value::Null,
                request_body: None,
                status: Some(200),
                mime_type: Some("application/json".into()),
                response_headers: None,
                started_at: 100.0,
            },
        );
        let event = json!({
            "method": "Network.loadingFinished",
            "params": {
                "requestId": "req1",
                "timestamp": 200.0
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert!(!state.network.contains_key("req1"));
        assert!(!actions.is_empty());
        assert!(actions.iter().any(|a| matches!(a, CdpAction::Emit(_))));
    }

    #[test]
    fn process_network_loading_finished_skips_body_for_non_fetch() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        state.sessions.insert(
            "s1".into(),
            PageSession {
                page_url: "http://localhost:3000".into(),
                host: "localhost:3000".into(),
            },
        );
        state.network.insert(
            "req1".into(),
            PendingNetwork {
                session_id: "s1".into(),
                request_id: "req1".into(),
                method: "GET".into(),
                url: "http://example.com/data".into(),
                resource_type: "Image".into(),
                request_headers: Value::Null,
                request_body: None,
                status: Some(200),
                mime_type: None,
                response_headers: None,
                started_at: 100.0,
            },
        );
        let event = json!({
            "method": "Network.loadingFinished",
            "params": {
                "requestId": "req1",
                "timestamp": 200.0
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert!(!actions
            .iter()
            .any(|a| matches!(a, CdpAction::FetchResponseBody { .. })));
    }

    #[test]
    fn process_network_loading_failed_emits_item() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        state.sessions.insert(
            "s1".into(),
            PageSession {
                page_url: "http://localhost:3000".into(),
                host: "localhost:3000".into(),
            },
        );
        state.network.insert(
            "req1".into(),
            PendingNetwork {
                session_id: "s1".into(),
                request_id: "req1".into(),
                method: "GET".into(),
                url: "http://api.example.com/data".into(),
                resource_type: "XHR".into(),
                request_headers: Value::Null,
                request_body: None,
                status: None,
                mime_type: None,
                response_headers: None,
                started_at: 100.0,
            },
        );
        let event = json!({
            "method": "Network.loadingFailed",
            "params": {
                "requestId": "req1",
                "errorText": "net::ERR_CONNECTION_REFUSED",
                "canceled": false
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert!(!state.network.contains_key("req1"));
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], CdpAction::Emit(_)));
    }

    #[test]
    fn process_network_loading_failed_with_canceled() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        state.sessions.insert(
            "s1".into(),
            PageSession {
                page_url: "http://localhost:3000".into(),
                host: "localhost:3000".into(),
            },
        );
        state.network.insert(
            "req1".into(),
            PendingNetwork {
                session_id: "s1".into(),
                request_id: "req1".into(),
                method: "GET".into(),
                url: "http://api.example.com/data".into(),
                resource_type: "XHR".into(),
                request_headers: Value::Null,
                request_body: None,
                status: None,
                mime_type: None,
                response_headers: None,
                started_at: 100.0,
            },
        );
        let event = json!({
            "method": "Network.loadingFailed",
            "params": {
                "requestId": "req1",
                "errorText": "net::ERR_ABORTED",
                "canceled": true
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert!(!state.network.contains_key("req1"));
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], CdpAction::Emit(_)));
    }

    #[test]
    fn process_unknown_method_no_action() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        let event = json!({
            "method": "Unknown.method",
            "params": {}
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert_eq!(actions.len(), 0);
    }

    #[test]
    fn process_events_without_session_id_skip() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        let event = json!({
            "method": "Runtime.consoleAPICalled",
            "params": {
                "type": "log",
                "args": []
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert_eq!(actions.len(), 0);
    }

    #[test]
    fn enqueue_handles_full_channel() {
        let (tx, mut rx) = mpsc::channel(1);
        let item1 = IngestItem {
            service: "test".into(),
            line: "msg1".into(),
        };
        let item2 = IngestItem {
            service: "test".into(),
            line: "msg2".into(),
        };
        enqueue(&tx, item1);
        enqueue(&tx, item2);
        let received = rx.try_recv().unwrap();
        assert_eq!(received.line, "msg1");
    }

    #[test]
    fn enqueue_handles_closed_channel() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        enqueue(
            &tx,
            IngestItem {
                service: "test".into(),
                line: "gone".into(),
            },
        );
    }

    #[test]
    fn process_network_request_skips_empty_request_id() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        state.sessions.insert(
            "s1".into(),
            PageSession {
                page_url: "http://localhost:3000".into(),
                host: "localhost:3000".into(),
            },
        );
        let event = json!({
            "method": "Network.requestWillBeSent",
            "sessionId": "s1",
            "params": {
                "requestId": "",
                "type": "XHR",
                "request": {"url": "http://example.com", "method": "GET"},
                "timestamp": 100.0
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert!(actions.is_empty());
        assert!(state.network.is_empty());
    }

    #[test]
    fn process_frame_navigated_without_session_id_skips() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        let event = json!({
            "method": "Page.frameNavigated",
            "params": {
                "frame": {"url": "http://new.com/page"}
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert!(actions.is_empty());
    }

    #[test]
    fn process_log_entry_without_session_id_skips() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        let event = json!({
            "method": "Log.entryAdded",
            "params": {
                "entry": {"level": "info", "text": "test", "timestamp": 1.0}
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert!(actions.is_empty());
    }

    #[test]
    fn process_exception_without_session_id_skips() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        let event = json!({
            "method": "Runtime.exceptionThrown",
            "params": {
                "exceptionDetails": {"text": "error", "timestamp": 1.0}
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert!(actions.is_empty());
    }

    #[test]
    fn process_network_request_without_session_id_skips() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        let event = json!({
            "method": "Network.requestWillBeSent",
            "params": {
                "requestId": "req1",
                "type": "XHR",
                "request": {"url": "http://example.com", "method": "GET"},
                "timestamp": 100.0
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert!(actions.is_empty());
        assert!(!state.network.contains_key("req1"));
    }

    #[test]
    fn process_network_loading_finished_requests_fetch_body_for_xhr() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        state.sessions.insert(
            "s1".into(),
            PageSession {
                page_url: "http://localhost:3000".into(),
                host: "localhost:3000".into(),
            },
        );
        state.network.insert(
            "req1".into(),
            PendingNetwork {
                session_id: "s1".into(),
                request_id: "req1".into(),
                method: "GET".into(),
                url: "http://api.example.com/data.json".into(),
                resource_type: "Fetch".into(),
                request_headers: Value::Null,
                request_body: None,
                status: Some(200),
                mime_type: Some("application/json".into()),
                response_headers: None,
                started_at: 100.0,
            },
        );
        let event = json!({
            "method": "Network.loadingFinished",
            "params": {
                "requestId": "req1",
                "timestamp": 200.0
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert!(actions.iter().any(|a| matches!(
            a,
            CdpAction::FetchResponseBody {
                request_id,
                session_id
            } if request_id == "req1" && session_id == "s1"
        )));
    }

    #[test]
    fn process_network_response_received_unknown_request_id_skips() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        let event = json!({
            "method": "Network.responseReceived",
            "params": {
                "requestId": "missing",
                "response": {"status": 404}
            }
        });
        let actions = process_cdp_event(&mut state, &event, false);
        assert!(actions.is_empty());
    }

    #[test]
    fn process_target_detached_cleans_network_for_session() {
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        state.sessions.insert(
            "s1".into(),
            PageSession {
                page_url: "http://example.com".into(),
                host: "example.com".into(),
            },
        );
        state.network.insert(
            "req1".into(),
            PendingNetwork {
                session_id: "s1".into(),
                request_id: "req1".into(),
                method: "GET".into(),
                url: "http://example.com".into(),
                resource_type: "XHR".into(),
                request_headers: Value::Null,
                request_body: None,
                status: None,
                mime_type: None,
                response_headers: None,
                started_at: 100.0,
            },
        );
        let event = json!({
            "method": "Target.detachedFromTarget",
            "params": {"sessionId": "s1"}
        });
        process_cdp_event(&mut state, &event, false);
        assert!(!state.sessions.contains_key("s1"));
        assert!(state.network.is_empty());
    }

    #[tokio::test]
    async fn cdp_call_returns_ok_result() {
        use futures_util::sink::drain;
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let next_id = Arc::new(AtomicU64::new(7));
        let write = Arc::new(Mutex::new(drain()));
        let pending_r = pending.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let mut map = pending_r.lock().await;
            if let Some(tx) = map.remove(&7) {
                let _ = tx.send(Ok(json!({"value": 1})));
            }
        });
        let result = cdp_call(
            &write,
            &pending,
            &next_id,
            "Runtime.evaluate",
            json!({"expression": "1"}),
            Some("sess-1"),
        )
        .await
        .unwrap();
        assert_eq!(result["value"], 1);
    }

    #[tokio::test]
    async fn cdp_call_propagates_error_response() {
        use futures_util::sink::drain;
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let next_id = Arc::new(AtomicU64::new(8));
        let write = Arc::new(Mutex::new(drain()));
        let pending_r = pending.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let mut map = pending_r.lock().await;
            if let Some(tx) = map.remove(&8) {
                let _ = tx.send(Err("method not found".into()));
            }
        });
        let err = cdp_call(&write, &pending, &next_id, "Bad.method", json!({}), None)
            .await
            .unwrap_err();
        assert_eq!(err, "method not found");
    }

    #[tokio::test]
    async fn cdp_call_times_out_when_no_response() {
        use futures_util::sink::drain;
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let next_id = Arc::new(AtomicU64::new(9));
        let write = Arc::new(Mutex::new(drain()));
        let err = cdp_call(&write, &pending, &next_id, "Slow.method", json!({}), None)
            .await
            .unwrap_err();
        assert!(err.contains("timeout"));
        assert!(err.contains("Slow.method"));
    }

    #[tokio::test]
    async fn cdp_call_send_failure() {
        use futures_util::sink::Sink;
        use std::pin::Pin;
        use std::task::{Context, Poll};

        struct FailSink;

        impl Sink<Message> for FailSink {
            type Error = std::io::Error;

            fn poll_ready(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }

            fn start_send(self: Pin<&mut Self>, _: Message) -> Result<(), Self::Error> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "send failed",
                ))
            }

            fn poll_flush(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }

            fn poll_close(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }
        }

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let next_id = Arc::new(AtomicU64::new(10));
        let write = Arc::new(Mutex::new(FailSink));
        let err = cdp_call(
            &write,
            &pending,
            &next_id,
            "Network.enable",
            json!({}),
            None,
        )
        .await
        .unwrap_err();
        assert!(err.contains("CDP send failed"));
    }

    #[tokio::test]
    async fn cdp_call_channel_closed_before_response() {
        use futures_util::sink::drain;
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let next_id = Arc::new(AtomicU64::new(11));
        let write = Arc::new(Mutex::new(drain()));
        let pending_r = pending.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let mut map = pending_r.lock().await;
            map.remove(&11);
        });
        let err = cdp_call(
            &write,
            &pending,
            &next_id,
            "Runtime.enable",
            json!({}),
            None,
        )
        .await
        .unwrap_err();
        assert!(err.contains("channel closed") || err.contains("timeout"));
    }

    fn auto_respond_cdp(pending: PendingMap) {
        tokio::spawn(async move {
            for _ in 0..50 {
                tokio::time::sleep(Duration::from_millis(5)).await;
                let mut map = pending.lock().await;
                let ids: Vec<u64> = map.keys().copied().collect();
                for id in ids {
                    if let Some(tx) = map.remove(&id) {
                        let _ = tx.send(Ok(json!({})));
                    }
                }
            }
        });
    }

    #[tokio::test]
    async fn dispatch_enable_domains_executes_cdp_calls() {
        use futures_util::sink::drain;
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let next_id = Arc::new(AtomicU64::new(1));
        let write = Arc::new(Mutex::new(drain()));
        auto_respond_cdp(pending.clone());
        let (tx, _rx) = mpsc::channel(4);
        let mut state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        state.sessions.insert(
            "s1".into(),
            PageSession {
                page_url: "http://localhost/app".into(),
                host: "localhost".into(),
            },
        );
        dispatch_cdp_actions(
            vec![CdpAction::EnableDomains {
                session_id: "s1".into(),
            }],
            &write,
            &pending,
            &next_id,
            &tx,
            &state,
        )
        .await;
    }

    #[tokio::test]
    async fn dispatch_detach_target_executes_cdp_calls() {
        use futures_util::sink::drain;
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let next_id = Arc::new(AtomicU64::new(1));
        let write = Arc::new(Mutex::new(drain()));
        auto_respond_cdp(pending.clone());
        let (tx, _rx) = mpsc::channel(4);
        let state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        dispatch_cdp_actions(
            vec![CdpAction::DetachTarget {
                session_id: "s1".into(),
            }],
            &write,
            &pending,
            &next_id,
            &tx,
            &state,
        )
        .await;
    }

    #[tokio::test]
    async fn dispatch_fetch_response_body_success_and_error() {
        use futures_util::sink::drain;
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let next_id = Arc::new(AtomicU64::new(1));
        let write = Arc::new(Mutex::new(drain()));
        let pending_ok = pending.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let mut map = pending_ok.lock().await;
            if let Some(tx) = map.remove(&1) {
                let _ = tx.send(Ok(json!({
                    "body": "hello",
                    "base64Encoded": false
                })));
            }
        });
        let (tx, _rx) = mpsc::channel(4);
        let state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        dispatch_cdp_actions(
            vec![CdpAction::FetchResponseBody {
                request_id: "req1".into(),
                session_id: "s1".into(),
            }],
            &write,
            &pending,
            &next_id,
            &tx,
            &state,
        )
        .await;

        let pending_err: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let next_err = Arc::new(AtomicU64::new(1));
        let write_err = Arc::new(Mutex::new(drain()));
        let pending_r = pending_err.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let mut map = pending_r.lock().await;
            if let Some(tx) = map.remove(&1) {
                let _ = tx.send(Err("no body".into()));
            }
        });
        dispatch_cdp_actions(
            vec![CdpAction::FetchResponseBody {
                request_id: "req2".into(),
                session_id: "s1".into(),
            }],
            &write_err,
            &pending_err,
            &next_err,
            &tx,
            &state,
        )
        .await;
    }

    #[tokio::test]
    async fn dispatch_emit_enqueues_item() {
        use futures_util::sink::drain;
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let next_id = Arc::new(AtomicU64::new(1));
        let write = Arc::new(Mutex::new(drain()));
        let (tx, mut rx) = mpsc::channel(4);
        let state = SessionState {
            sessions: HashMap::new(),
            network: HashMap::new(),
        };
        dispatch_cdp_actions(
            vec![CdpAction::Emit(IngestItem {
                service: "browser".into(),
                line: r#"{"msg":"hi"}"#.into(),
            })],
            &write,
            &pending,
            &next_id,
            &tx,
            &state,
        )
        .await;
        let item = rx.try_recv().unwrap();
        assert_eq!(item.service, "browser");
    }
}
