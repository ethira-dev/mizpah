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
pub(crate) struct WsSubscription {
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

pub(crate) fn in_time_range(
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

pub(crate) fn event_matches_subscription(event: &WsEvent, sub: &WsSubscription) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{LogEntry, WsEvent};
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    #[test]
    fn in_time_range_all_pass() {
        let ts = Utc.with_ymd_and_hms(2024, 1, 15, 12, 0, 0).unwrap();
        assert!(in_time_range(ts, None, None));
    }

    #[test]
    fn in_time_range_from_bound() {
        let ts = Utc.with_ymd_and_hms(2024, 1, 15, 12, 0, 0).unwrap();
        let from = Utc.with_ymd_and_hms(2024, 1, 10, 0, 0, 0).unwrap();
        assert!(in_time_range(ts, Some(from), None));
        let from_after = Utc.with_ymd_and_hms(2024, 1, 20, 0, 0, 0).unwrap();
        assert!(!in_time_range(ts, Some(from_after), None));
    }

    #[test]
    fn in_time_range_to_bound() {
        let ts = Utc.with_ymd_and_hms(2024, 1, 15, 12, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2024, 1, 20, 0, 0, 0).unwrap();
        assert!(in_time_range(ts, None, Some(to)));
        let to_exact = ts;
        assert!(!in_time_range(ts, None, Some(to_exact)));
        let to_before = Utc.with_ymd_and_hms(2024, 1, 10, 0, 0, 0).unwrap();
        assert!(!in_time_range(ts, None, Some(to_before)));
    }

    #[test]
    fn in_time_range_both_bounds() {
        let ts = Utc.with_ymd_and_hms(2024, 1, 15, 12, 0, 0).unwrap();
        let from = Utc.with_ymd_and_hms(2024, 1, 10, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2024, 1, 20, 0, 0, 0).unwrap();
        assert!(in_time_range(ts, Some(from), Some(to)));
    }

    #[test]
    fn event_matches_sub_wildcard_service() {
        let entry = LogEntry {
            id: 1,
            service: "api".into(),
            received_at: Utc::now(),
            event_time: None,
            format_id: None,
            data: json!({"msg": "test"}),
            approx_bytes: 0,
        };
        let sub = WsSubscription {
            service: "*".into(),
            query: CompiledQuery::MatchAll,
            from: None,
            to: None,
        };
        assert!(event_matches_subscription(
            &WsEvent::Log { entry },
            &sub
        ));
    }

    #[test]
    fn event_matches_sub_service_filter() {
        let entry = LogEntry {
            id: 1,
            service: "api".into(),
            received_at: Utc::now(),
            event_time: None,
            format_id: None,
            data: json!({"msg": "test"}),
            approx_bytes: 0,
        };
        let sub = WsSubscription {
            service: "api".into(),
            query: CompiledQuery::MatchAll,
            from: None,
            to: None,
        };
        assert!(event_matches_subscription(
            &WsEvent::Log { entry: entry.clone() },
            &sub
        ));

        let sub_other = WsSubscription {
            service: "backend".into(),
            query: CompiledQuery::MatchAll,
            from: None,
            to: None,
        };
        assert!(!event_matches_subscription(
            &WsEvent::Log { entry },
            &sub_other
        ));
    }

    #[test]
    fn event_matches_sub_time_bounds() {
        let ts = Utc.with_ymd_and_hms(2024, 1, 15, 12, 0, 0).unwrap();
        let entry = LogEntry {
            id: 1,
            service: "api".into(),
            received_at: ts,
            event_time: None,
            format_id: None,
            data: json!({"msg": "test"}),
            approx_bytes: 0,
        };

        let from = Utc.with_ymd_and_hms(2024, 1, 10, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2024, 1, 20, 0, 0, 0).unwrap();
        let sub = WsSubscription {
            service: "*".into(),
            query: CompiledQuery::MatchAll,
            from: Some(from),
            to: Some(to),
        };
        assert!(event_matches_subscription(
            &WsEvent::Log { entry: entry.clone() },
            &sub
        ));

        let sub_out = WsSubscription {
            service: "*".into(),
            query: CompiledQuery::MatchAll,
            from: Some(Utc.with_ymd_and_hms(2024, 2, 1, 0, 0, 0).unwrap()),
            to: None,
        };
        assert!(!event_matches_subscription(
            &WsEvent::Log { entry },
            &sub_out
        ));
    }

    #[test]
    fn event_matches_sub_query() {
        let entry_error = LogEntry {
            id: 1,
            service: "api".into(),
            received_at: Utc::now(),
            event_time: None,
            format_id: None,
            data: json!({"level": "error", "msg": "boom"}),
            approx_bytes: 0,
        };
        let entry_info = LogEntry {
            id: 2,
            service: "api".into(),
            received_at: Utc::now(),
            event_time: None,
            format_id: None,
            data: json!({"level": "info", "msg": "ok"}),
            approx_bytes: 0,
        };

        let query = compile_query(r#"level == "error""#).unwrap();
        let sub = WsSubscription {
            service: "*".into(),
            query,
            from: None,
            to: None,
        };

        assert!(event_matches_subscription(
            &WsEvent::Log { entry: entry_error },
            &sub
        ));
        assert!(!event_matches_subscription(
            &WsEvent::Log { entry: entry_info },
            &sub
        ));
    }

    #[test]
    fn event_matches_empty_service_filter() {
        let entry = LogEntry {
            id: 1,
            service: "any".into(),
            received_at: Utc::now(),
            event_time: None,
            format_id: None,
            data: json!({"msg": "test"}),
            approx_bytes: 0,
        };
        let sub = WsSubscription {
            service: String::new(),
            query: CompiledQuery::MatchAll,
            from: None,
            to: None,
        };
        assert!(event_matches_subscription(
            &WsEvent::Log { entry },
            &sub
        ));
    }

    #[test]
    fn event_matches_non_log_always_true() {
        let sub = WsSubscription {
            service: "api".into(),
            query: CompiledQuery::MatchAll,
            from: None,
            to: None,
        };
        assert!(event_matches_subscription(&WsEvent::Pong, &sub));
        assert!(event_matches_subscription(
            &WsEvent::Services {
                names: vec![],
                blocked: vec![]
            },
            &sub
        ));
        assert!(event_matches_subscription(&WsEvent::Lagged { skipped: 10 }, &sub));
    }

    // Integration tests with tokio-tungstenite
    #[cfg(not(miri))]
    use crate::test_support::spawn_test_hub;
    #[cfg(not(miri))]
    use futures_util::{SinkExt, StreamExt};
    #[cfg(not(miri))]
    use tokio_tungstenite::tungstenite::Message as WsMessage;
    #[cfg(not(miri))]
    use std::time::Duration;

    #[cfg(not(miri))]
    #[tokio::test]
    async fn ws_receives_services_snapshot() {
        let (url, _store) = spawn_test_hub().await;
        let ws_url = url.replace("http://", "ws://") + "/ws";
        let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .unwrap();

        let msg = ws.next().await.unwrap().unwrap();
        if let WsMessage::Text(txt) = msg {
            let val: serde_json::Value = serde_json::from_str(&txt).unwrap();
            assert_eq!(val.get("type").and_then(|v| v.as_str()), Some("services"));
        } else {
            panic!("expected text message");
        }
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn ws_subscribe_filters_logs() {
        let (url, store) = spawn_test_hub().await;
        let ws_url = url.replace("http://", "ws://") + "/ws";
        let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .unwrap();

        // Consume services snapshot
        let _ = ws.next().await.unwrap().unwrap();

        // Subscribe to error-level logs
        let sub = json!({
            "type": "subscribe",
            "service": "*",
            "q": r#"level == "error""#
        });
        ws.send(WsMessage::Text(sub.to_string().into())).await.unwrap();
        // Allow the server to apply the subscription before pushing.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Push logs
        store
            .push_line("api", r#"{"level":"info","msg":"ok"}"#)
            .await;
        store
            .push_line("api", r#"{"level":"error","msg":"boom"}"#)
            .await;

        // Drain until we see the filtered error log (services/properties may interleave).
        let mut found_msg = None;
        for _ in 0..20 {
            let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
                .await
                .ok()
                .flatten()
                .and_then(|r| r.ok());
            let Some(WsMessage::Text(txt)) = msg else {
                continue;
            };
            let Ok(val) = serde_json::from_str::<serde_json::Value>(&txt) else {
                continue;
            };
            if val.get("type").and_then(|v| v.as_str()) == Some("log") {
                found_msg = val
                    .get("entry")
                    .and_then(|e| e.get("data"))
                    .and_then(|d| d.get("msg"))
                    .and_then(|m| m.as_str())
                    .map(|s| s.to_string());
                break;
            }
        }
        assert_eq!(found_msg.as_deref(), Some("boom"));
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn ws_ping_pong() {
        let (url, _store) = spawn_test_hub().await;
        let ws_url = url.replace("http://", "ws://") + "/ws";
        let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .unwrap();

        // Consume services snapshot
        let _ = ws.next().await.unwrap().unwrap();

        // Send ping
        let ping = json!({"type": "ping"});
        ws.send(WsMessage::Text(ping.to_string().into())).await.unwrap();

        // Should receive pong
        let msg = ws.next().await.unwrap().unwrap();
        if let WsMessage::Text(txt) = msg {
            let val: serde_json::Value = serde_json::from_str(&txt).unwrap();
            assert_eq!(val.get("type").and_then(|v| v.as_str()), Some("pong"));
        } else {
            panic!("expected pong");
        }
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn ws_invalid_cel_ignored() {
        let (url, store) = spawn_test_hub().await;
        let ws_url = url.replace("http://", "ws://") + "/ws";
        let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .unwrap();

        // Consume services snapshot
        let _ = ws.next().await.unwrap().unwrap();

        // Send invalid CEL - should be ignored
        let bad_sub = json!({
            "type": "subscribe",
            "q": "this is not valid CEL (((("
        });
        ws.send(WsMessage::Text(bad_sub.to_string().into()))
            .await
            .unwrap();

        // Push log - should still receive it with default MatchAll
        store
            .push_line("api", r#"{"level":"info","msg":"ok"}"#)
            .await;

        assert!(
            recv_log_type(&mut ws).await,
            "expected log after invalid CEL"
        );
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn ws_invalid_time_bounds_ignored() {
        let (url, store) = spawn_test_hub().await;
        let ws_url = url.replace("http://", "ws://") + "/ws";
        let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .unwrap();

        // Consume services snapshot
        let _ = ws.next().await.unwrap().unwrap();

        // Send invalid time bounds
        let bad_sub = json!({
            "type": "subscribe",
            "from": "not-a-date",
            "to": "also-not-a-date"
        });
        ws.send(WsMessage::Text(bad_sub.to_string().into()))
            .await
            .unwrap();

        // Push log - should still receive it
        store
            .push_line("api", r#"{"level":"info","msg":"ok"}"#)
            .await;

        assert!(
            recv_log_type(&mut ws).await,
            "expected log after invalid time bounds"
        );
    }

    #[cfg(not(miri))]
    async fn recv_log_type(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> bool {
        for _ in 0..20 {
            let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
                .await
                .ok()
                .flatten()
                .and_then(|r| r.ok());
            let Some(WsMessage::Text(txt)) = msg else {
                continue;
            };
            let Ok(val) = serde_json::from_str::<serde_json::Value>(&txt) else {
                continue;
            };
            if val.get("type").and_then(|v| v.as_str()) == Some("log") {
                return true;
            }
        }
        false
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn ws_close_terminates_connection() {
        let (url, _store) = spawn_test_hub().await;
        let ws_url = url.replace("http://", "ws://") + "/ws";
        let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .unwrap();

        // Consume services snapshot
        let _ = ws.next().await.unwrap().unwrap();

        // Send close
        ws.close(None).await.ok();

        // Connection should end (None) or echo Close / Err after peer closes.
        let result = tokio::time::timeout(Duration::from_secs(2), ws.next()).await;
        match result {
            Ok(None) | Ok(Some(Ok(WsMessage::Close(_)))) | Ok(Some(Err(_))) => {}
            other => panic!("unexpected close result: {other:?}"),
        }
    }
}
