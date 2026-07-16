use crate::filter::{compile_query, matches_entry, CompiledQuery};
use crate::investigate::{self, InvestigateTarget};
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
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::watch;
use tower_http::cors::CorsLayer;
use tracing::warn;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub project_dir: PathBuf,
}

#[derive(Embed)]
#[folder = "static/"]
#[prefix = ""]
struct Assets;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/ingest", post(ingest))
        .route("/api/ingest/batch", post(ingest_batch))
        .route("/api/logs", get(list_logs))
        .route("/api/services", get(list_services))
        .route("/api/properties", get(list_properties))
        .route("/api/stats", get(stats))
        .route("/api/investigate", post(investigate))
        .route("/ws", get(ws_handler))
        .fallback(static_handler)
        .layer(CorsLayer::permissive())
        .with_state(state)
}

const INGEST_BATCH_MAX: usize = 128;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IngestRequest {
    service: String,
    line: String,
    #[serde(default)]
    cmd: Option<String>,
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
    let entries = state
        .store
        .push_line_with_meta(&body.service, &body.line, body.cmd.as_deref())
        .await;
    Ok(Json(IngestResponse { entries }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IngestBatchRequest {
    service: String,
    lines: Vec<String>,
    #[serde(default)]
    cmd: Option<String>,
}

async fn ingest_batch(
    State(state): State<AppState>,
    Json(body): Json<IngestBatchRequest>,
) -> Result<Json<IngestResponse>, (StatusCode, String)> {
    if body.service.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "service is required".into()));
    }
    if body.lines.len() > INGEST_BATCH_MAX {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("at most {INGEST_BATCH_MAX} lines per batch"),
        ));
    }
    let cmd = body.cmd.as_deref();
    let mut entries = Vec::new();
    for line in &body.lines {
        entries.extend(
            state
                .store
                .push_line_with_meta(&body.service, line, cmd)
                .await,
        );
    }
    Ok(Json(IngestResponse { entries }))
}

#[derive(Debug, Deserialize)]
struct LogsQuery {
    service: Option<String>,
    cursor: Option<u64>,
    limit: Option<usize>,
    /// CEL filter expression
    q: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LogsResponse {
    entries: Vec<LogEntry>,
    has_more: bool,
}

async fn list_logs(
    State(state): State<AppState>,
    Query(params): Query<LogsQuery>,
) -> Result<Json<LogsResponse>, (StatusCode, String)> {
    let query = compile_query(params.q.as_deref().unwrap_or(""))
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let (entries, has_more) = state
        .store
        .query_logs(
            params.service.as_deref(),
            params.cursor,
            params.limit.unwrap_or(100),
            &query,
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
    /// Case-insensitive substring match against property paths and sample values.
    q: Option<String>,
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
        properties: state
            .store
            .search_properties(q.service.as_deref(), q.q.as_deref())
            .await,
    })
}

async fn stats(State(state): State<AppState>) -> Json<Stats> {
    Json(state.store.stats().await)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InvestigateRequest {
    target: InvestigateTarget,
    id: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InvestigateResponse {
    ok: bool,
}

async fn investigate(
    State(state): State<AppState>,
    Json(body): Json<InvestigateRequest>,
) -> Result<Json<InvestigateResponse>, (StatusCode, String)> {
    let entry = state.store.get_entry(body.id).await.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("log entry {} not found", body.id),
        )
    })?;

    let project_dir = state.project_dir.clone();
    let target = body.target;
    tokio::task::spawn_blocking(move || investigate::launch_session(target, &entry, &project_dir))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("investigate task failed: {e}"),
            )
        })?
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;

    Ok(Json(InvestigateResponse { ok: true }))
}

#[derive(Clone)]
struct WsSubscription {
    /// `*` or empty means all services.
    service: String,
    query: CompiledQuery,
}

impl Default for WsSubscription {
    fn default() -> Self {
        Self {
            service: "*".into(),
            query: CompiledQuery::MatchAll,
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
            matches_entry(&entry.service, &entry.data, &sub.query)
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
                if let Ok(WsClientMessage::Subscribe { service, q }) =
                    serde_json::from_str::<WsClientMessage>(&text)
                {
                    match compile_query(&q) {
                        Ok(query) => {
                            let _ = sub_tx.send(WsSubscription { service, query });
                        }
                        Err(err) => {
                            warn!(error = %err, "ignoring invalid WS CEL query");
                        }
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

    fn test_state(store: Arc<Store>) -> AppState {
        AppState {
            store,
            project_dir: std::env::temp_dir(),
        }
    }

    fn test_app() -> Router {
        router(test_state(Arc::new(Store::new(1_000_000))))
    }

    #[tokio::test]
    async fn ingest_single_line() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ingest")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"service":"api","line":"{\"msg\":\"hi\"}"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn ingest_batch_ordered() {
        let store = Arc::new(Store::new(1_000_000));
        let app = router(test_state(Arc::clone(&store)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ingest/batch")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "service": "shell",
                            "lines": [
                                "{\"n\":1}",
                                "{\"n\":2}",
                                "plain"
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let (entries, _) = store
            .query_logs(Some("shell"), None, 10, &CompiledQuery::MatchAll)
            .await;
        assert_eq!(entries.len(), 3);
        // Newest first
        assert_eq!(entries[0].data["_raw"], json!("plain"));
        assert_eq!(entries[1].data["n"], json!(2));
        assert_eq!(entries[2].data["n"], json!(1));
    }

    #[tokio::test]
    async fn ingest_batch_injects_cmd() {
        let store = Arc::new(Store::new(1_000_000));
        let app = router(test_state(Arc::clone(&store)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ingest/batch")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "service": "/Users/me/app",
                            "cmd": "npm test",
                            "lines": [
                                "{\"msg\":\"hi\"}",
                                "plain"
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let (entries, _) = store
            .query_logs(Some("/Users/me/app"), None, 10, &CompiledQuery::MatchAll)
            .await;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].data["cmd"], json!("npm test"));
        assert_eq!(entries[0].data["_raw"], json!("plain"));
        assert_eq!(entries[1].data["cmd"], json!("npm test"));
        assert_eq!(entries[1].data["msg"], json!("hi"));
    }

    #[tokio::test]
    async fn ingest_batch_rejects_empty_service() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ingest/batch")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"service":"","lines":["a"]}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn ingest_batch_rejects_oversized() {
        let app = test_app();
        let lines: Vec<String> = (0..129).map(|i| format!("line{i}")).collect();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ingest/batch")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"service":"s","lines": lines}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn investigate_missing_entry_is_404() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/investigate")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"target":"claude","id":999}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
