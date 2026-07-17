//! WebSocket live log subscription.

use super::routes::parse_rfc3339;
use super::AppState;
use crate::filter::{compile_query, matches_entry, CompiledQuery};
use crate::store::WsEvent;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::watch;
use tracing::warn;

#[derive(Clone)]
struct WsSubscription {
    /// `*` or empty means all services.
    service: String,
    query: CompiledQuery,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
}

impl Default for WsSubscription {
    fn default() -> Self {
        Self {
            service: "*".into(),
            query: CompiledQuery::MatchAll,
            from: None,
            to: None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum WsClientMessage {
    #[serde(rename = "subscribe")]
    Subscribe {
        #[serde(default = "default_service")]
        service: String,
        #[serde(default)]
        q: String,
        #[serde(default)]
        from: Option<String>,
        #[serde(default)]
        to: Option<String>,
    },
    #[serde(rename = "ping")]
    Ping,
}

fn default_service() -> String {
    "*".into()
}

fn in_time_range(
    ts: DateTime<Utc>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> bool {
    if let Some(from) = from {
        if ts < from {
            return false;
        }
    }
    if let Some(to) = to {
        if ts >= to {
            return false;
        }
    }
    true
}

fn event_matches_subscription(event: &WsEvent, sub: &WsSubscription) -> bool {
    match event {
        WsEvent::Log { entry } => {
            let service_ok =
                sub.service.is_empty() || sub.service == "*" || entry.service == sub.service;
            if !service_ok {
                return false;
            }
            if !in_time_range(entry.received_at, sub.from, sub.to) {
                return false;
            }
            matches_entry(&entry.service, &entry.data, &sub.query)
        }
        _ => true,
    }
}

pub(crate) async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = state.store.subscribe();

    let (sub_tx, sub_rx) = watch::channel(WsSubscription::default());
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<WsEvent>();

    // Send initial services snapshot
    let names = state.store.service_names().await;
    let blocked = state.store.blocked_names().await;
    let init = WsEvent::Services { names, blocked };
    if let Ok(json) = serde_json::to_string(&init) {
        if sender.send(Message::Text(json.into())).await.is_err() {
            return;
        }
    }

    let send_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                event = rx.recv() => {
                    match event {
                        Ok(event) => {
                            let sub = sub_rx.borrow().clone();
                            if !event_matches_subscription(&event, &sub) {
                                continue;
                            }
                            match serde_json::to_string(&event) {
                                Ok(json) => {
                                    if sender.send(Message::Text(json.into())).await.is_err() {
                                        break;
                                    }
                                }
                                Err(err) => {
                                    warn!(error = %err, "failed to serialize ws event");
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!(skipped = n, "websocket subscriber lagged");
                            let lagged = WsEvent::Lagged { skipped: n };
                            match serde_json::to_string(&lagged) {
                                Ok(json) => {
                                    if sender.send(Message::Text(json.into())).await.is_err() {
                                        break;
                                    }
                                }
                                Err(err) => {
                                    warn!(error = %err, "failed to serialize lagged event");
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                event = out_rx.recv() => {
                    match event {
                        Some(event) => {
                            match serde_json::to_string(&event) {
                                Ok(json) => {
                                    if sender.send(Message::Text(json.into())).await.is_err() {
                                        break;
                                    }
                                }
                                Err(err) => {
                                    warn!(error = %err, "failed to serialize ws event");
                                }
                            }
                        }
                        None => break,
                    }
                }
            }
        }
    });

    while let Some(Ok(msg)) = receiver.next().await {
        match msg {
            Message::Close(_) => break,
            Message::Text(text) => {
                let Ok(client_msg) = serde_json::from_str::<WsClientMessage>(&text) else {
                    continue;
                };
                match client_msg {
                    WsClientMessage::Ping => {
                        let _ = out_tx.send(WsEvent::Pong);
                    }
                    WsClientMessage::Subscribe {
                        service,
                        q,
                        from,
                        to,
                    } => match compile_query(&q) {
                        Ok(query) => {
                            let from = match parse_rfc3339("from", from.as_deref()) {
                                Ok(v) => v,
                                Err(err) => {
                                    warn!(error = %err, "ignoring invalid WS from");
                                    None
                                }
                            };
                            let to = match parse_rfc3339("to", to.as_deref()) {
                                Ok(v) => v,
                                Err(err) => {
                                    warn!(error = %err, "ignoring invalid WS to");
                                    None
                                }
                            };
                            let _ = sub_tx.send(WsSubscription {
                                service,
                                query,
                                from,
                                to,
                            });
                        }
                        Err(err) => {
                            warn!(error = %err, "ignoring invalid WS CEL query");
                        }
                    },
                }
            }
            _ => {}
        }
    }

    send_task.abort();
}
