use crate::filter::{parse_filters_param, FilterChip};
use crate::store::{LogEntry, PropertyInfo, Stats, Store, WsEvent};
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Query, State, WebSocketUpgrade};
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::watch;
use tower_http::cors::CorsLayer;
use tracing::warn;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
}

#[derive(Embed)]
#[folder = "static/"]
#[prefix = ""]
struct Assets;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/ingest", post(ingest))
        .route("/api/logs", get(list_logs))
        .route("/api/services", get(list_services))
        .route("/api/properties", get(list_properties))
        .route("/api/stats", get(stats))
        .route("/ws", get(ws_handler))
        .fallback(static_handler)
        .layer(CorsLayer::permissive())
        .with_state(state)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IngestRequest {
    service: String,
    line: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IngestResponse {
    /// Entries emitted by this line (empty while buffering a pretty block).
    entries: Vec<LogEntry>,
}

async fn ingest(
    State(state): State<AppState>,
    Json(body): Json<IngestRequest>,
) -> Result<Json<IngestResponse>, (StatusCode, String)> {
    if body.service.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "service is required".into()));
    }
    let entries = state.store.push_line(&body.service, &body.line).await;
    Ok(Json(IngestResponse { entries }))
}

#[derive(Debug, Deserialize)]
struct LogsQuery {
    service: Option<String>,
    cursor: Option<u64>,
    limit: Option<usize>,
    /// JSON array of FilterChip
    filters: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LogsResponse {
    entries: Vec<LogEntry>,
    has_more: bool,
}

async fn list_logs(
    State(state): State<AppState>,
    Query(q): Query<LogsQuery>,
) -> Result<Json<LogsResponse>, (StatusCode, String)> {
    let filters: Vec<FilterChip> =
        parse_filters_param(q.filters.as_deref()).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let (entries, has_more) = state
        .store
        .query_logs(
            q.service.as_deref(),
            q.cursor,
            q.limit.unwrap_or(100),
            &filters,
        )
        .await;
    Ok(Json(LogsResponse { entries, has_more }))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServicesResponse {
    services: Vec<String>,
}

async fn list_services(State(state): State<AppState>) -> Json<ServicesResponse> {
    Json(ServicesResponse {
        services: state.store.service_names().await,
    })
}

#[derive(Debug, Deserialize)]
struct PropertiesQuery {
    service: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PropertiesResponse {
    properties: Vec<PropertyInfo>,
}

async fn list_properties(
    State(state): State<AppState>,
    Query(q): Query<PropertiesQuery>,
) -> Json<PropertiesResponse> {
    Json(PropertiesResponse {
        properties: state.store.properties(q.service.as_deref()).await,
    })
}

async fn stats(State(state): State<AppState>) -> Json<Stats> {
    Json(state.store.stats().await)
}

#[derive(Debug, Clone, Default)]
struct WsSubscription {
    /// `*` or empty means all services.
    service: String,
    filters: Vec<FilterChip>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum WsClientMessage {
    #[serde(rename = "subscribe")]
    Subscribe {
        #[serde(default = "default_service")]
        service: String,
        #[serde(default)]
        filters: Vec<FilterChip>,
    },
}

fn default_service() -> String {
    "*".into()
}

fn event_matches_subscription(event: &WsEvent, sub: &WsSubscription) -> bool {
    match event {
        WsEvent::Log { entry } => {
            let service_ok =
                sub.service.is_empty() || sub.service == "*" || entry.service == sub.service;
            if !service_ok {
                return false;
            }
            crate::filter::matches_all(&entry.service, &entry.data, &sub.filters)
        }
        _ => true,
    }
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = state.store.subscribe();

    let (sub_tx, sub_rx) = watch::channel(WsSubscription::default());

    // Send initial services snapshot
    let names = state.store.service_names().await;
    let init = WsEvent::Services { names };
    if let Ok(json) = serde_json::to_string(&init) {
        if sender.send(Message::Text(json.into())).await.is_err() {
            return;
        }
    }

    let send_task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
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
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    while let Some(Ok(msg)) = receiver.next().await {
        match msg {
            Message::Close(_) => break,
            Message::Text(text) => {
                if let Ok(WsClientMessage::Subscribe { service, filters }) =
                    serde_json::from_str::<WsClientMessage>(&text)
                {
                    let _ = sub_tx.send(WsSubscription { service, filters });
                }
            }
            _ => {}
        }
    }

    send_task.abort();
}

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match Assets::get(path) {
        Some(file) => {
            let mime = file.metadata.mimetype();
            ([(header::CONTENT_TYPE, mime)], file.data).into_response()
        }
        None => {
            // SPA fallback
            match Assets::get("index.html") {
                Some(file) => {
                    let mime = file.metadata.mimetype();
                    ([(header::CONTENT_TYPE, mime)], file.data).into_response()
                }
                None => (
                    StatusCode::NOT_FOUND,
                    "UI assets not found. Build the web UI and copy into crates/mizpah/static/.",
                )
                    .into_response(),
            }
        }
    }
}
