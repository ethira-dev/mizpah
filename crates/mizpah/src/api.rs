use crate::filter::{compile_query, matches_entry, CompiledQuery};
use crate::investigate::{self, InvestigateTarget};
use crate::mzp_meta::MzpMeta;
use crate::store::{ActivityBucket, LogEntry, PropertyInfo, Stats, Store, WsEvent};
use crate::update::{self, ApplyBeginError, UpdateEvent, UpdateManager};
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{ConnectInfo, FromRequestParts, Query, State, WebSocketUpgrade};
use axum::http::request::Parts;
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures_util::stream;
use futures_util::{SinkExt, StreamExt};
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tower_http::cors::CorsLayer;
use tracing::warn;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub project_dir: PathBuf,
    pub update: Arc<UpdateManager>,
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
        .route("/api/services/disconnect", post(disconnect_service))
        .route("/api/services/reconnect", post(reconnect_service))
        .route("/api/properties", get(list_properties))
        .route("/api/stats", get(stats))
        .route("/api/activity", get(activity))
        .route("/api/investigate", post(investigate))
        .route("/api/update", get(get_update).post(post_update))
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
    #[serde(default)]
    mzp: Option<MzpMeta>,
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
    if state.store.is_blocked(&body.service).await {
        return Err((StatusCode::CONFLICT, "service disconnected".into()));
    }
    // Prefer client-provided receiver meta; fall back to hub process so every row has `_mzp`.
    let mzp = body.mzp.unwrap_or_else(MzpMeta::capture);
    let entries = state
        .store
        .push_line_with_meta(&body.service, &body.line, body.cmd.as_deref(), Some(&mzp))
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
    #[serde(default)]
    mzp: Option<MzpMeta>,
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
    if state.store.is_blocked(&body.service).await {
        return Err((StatusCode::CONFLICT, "service disconnected".into()));
    }
    let cmd = body.cmd.as_deref();
    let mzp = body.mzp.unwrap_or_else(MzpMeta::capture);
    let mut entries = Vec::new();
    for line in &body.lines {
        entries.extend(
            state
                .store
                .push_line_with_meta(&body.service, line, cmd, Some(&mzp))
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
    /// Inclusive lower bound (RFC3339).
    from: Option<String>,
    /// Exclusive upper bound (RFC3339).
    to: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LogsResponse {
    entries: Vec<LogEntry>,
    has_more: bool,
}

fn parse_rfc3339(label: &str, value: Option<&str>) -> Result<Option<DateTime<Utc>>, String> {
    match value.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => DateTime::parse_from_rfc3339(s)
            .map(|dt| Some(dt.with_timezone(&Utc)))
            .map_err(|e| format!("invalid {label}: {e}")),
    }
}

async fn list_logs(
    State(state): State<AppState>,
    Query(params): Query<LogsQuery>,
) -> Result<Json<LogsResponse>, (StatusCode, String)> {
    let query = compile_query(params.q.as_deref().unwrap_or(""))
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let from = parse_rfc3339("from", params.from.as_deref())
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let to =
        parse_rfc3339("to", params.to.as_deref()).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let (entries, has_more) = state
        .store
        .query_logs(
            params.service.as_deref(),
            params.cursor,
            params.limit.unwrap_or(100),
            &query,
            from,
            to,
        )
        .await;
    Ok(Json(LogsResponse { entries, has_more }))
}

#[derive(Debug, Deserialize)]
struct ActivityQuery {
    /// Trailing window in hours (default 24).
    hours: Option<u64>,
    /// Bucket size in minutes (default 25).
    #[serde(rename = "bucketMinutes")]
    bucket_minutes: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ActivityResponse {
    buckets: Vec<ActivityBucket>,
}

async fn activity(
    State(state): State<AppState>,
    Query(params): Query<ActivityQuery>,
) -> Json<ActivityResponse> {
    // Up to ~31 days window; buckets from 1 minute to 1 day.
    let hours = params.hours.unwrap_or(24).clamp(1, 744);
    let bucket_minutes = params.bucket_minutes.unwrap_or(25).clamp(1, 1440);
    let buckets = state
        .store
        .activity_histogram(
            ChronoDuration::hours(hours as i64),
            ChronoDuration::minutes(bucket_minutes as i64),
        )
        .await;
    Json(ActivityResponse { buckets })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServicesResponse {
    services: Vec<String>,
    blocked: Vec<String>,
}

async fn list_services(State(state): State<AppState>) -> Json<ServicesResponse> {
    Json(ServicesResponse {
        services: state.store.service_names().await,
        blocked: state.store.blocked_names().await,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServiceNameRequest {
    service: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DisconnectResponse {
    service: String,
    evicted: usize,
}

async fn disconnect_service(
    State(state): State<AppState>,
    Json(body): Json<ServiceNameRequest>,
) -> Result<Json<DisconnectResponse>, (StatusCode, String)> {
    if body.service.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "service is required".into()));
    }
    let evicted = state.store.disconnect_service(&body.service).await;
    Ok(Json(DisconnectResponse {
        service: body.service,
        evicted: evicted.len(),
    }))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReconnectResponse {
    service: String,
    reconnected: bool,
}

async fn reconnect_service(
    State(state): State<AppState>,
    Json(body): Json<ServiceNameRequest>,
) -> Result<Json<ReconnectResponse>, (StatusCode, String)> {
    if body.service.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "service is required".into()));
    }
    let reconnected = state.store.reconnect_service(&body.service).await;
    Ok(Json(ReconnectResponse {
        service: body.service,
        reconnected,
    }))
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

async fn get_update(State(state): State<AppState>) -> Json<update::UpdateStatus> {
    Json(state.update.status().await)
}

/// Peer address for privileged routes. Missing connect info is treated as non-loopback.
struct PeerAddr(SocketAddr);

impl<S> FromRequestParts<S> for PeerAddr
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        if let Some(ConnectInfo(addr)) = parts.extensions.get::<ConnectInfo<SocketAddr>>() {
            return Ok(PeerAddr(*addr));
        }
        Ok(PeerAddr(SocketAddr::from(([203, 0, 113, 1], 0))))
    }
}

async fn post_update(
    PeerAddr(peer): PeerAddr,
    State(state): State<AppState>,
) -> Result<Response, (StatusCode, String)> {
    if !peer.ip().is_loopback() {
        return Err((
            StatusCode::FORBIDDEN,
            "updates are only allowed from localhost".into(),
        ));
    }

    let latest = match state.update.try_begin_apply().await {
        Ok(v) => v,
        Err(ApplyBeginError::Busy) => {
            return Err((
                StatusCode::CONFLICT,
                "an update is already in progress".into(),
            ));
        }
        Err(ApplyBeginError::NoUpdate) => {
            return Err((StatusCode::BAD_REQUEST, "no update available".into()));
        }
    };

    let (tx, rx) = mpsc::unbounded_channel::<UpdateEvent>();
    let _ = tx.send(UpdateEvent {
        step: "Starting update…".into(),
        progress: 0.0,
        error: None,
        restarting: None,
    });

    let manager = Arc::clone(&state.update);
    tokio::spawn(async move {
        update::apply_update(manager, latest, tx).await;
    });

    let stream = stream::unfold((rx, false), |(mut rx, done)| async move {
        if done {
            return None;
        }
        match rx.recv().await {
            Some(ev) => {
                let terminal = ev.error.is_some() || ev.restarting == Some(true);
                let data = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".into());
                let item = Ok::<_, Infallible>(Event::default().data(data));
                Some((item, (rx, terminal)))
            }
            None => None,
        }
    });

    let mut res = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response();
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    );
    res.headers_mut().insert(
        "x-accel-buffering",
        HeaderValue::from_static("no"),
    );
    Ok(res)
}

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

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
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
            update: update::UpdateManager::new(update::RestartContext {
                host: "127.0.0.1".into(),
                port: 1738,
                project_dir: std::env::temp_dir(),
                max_bytes: 1_000_000,
            }),
        }
    }

    fn test_app() -> Router {
        router(test_state(Arc::new(Store::new(1_000_000))))
    }

    #[tokio::test]
    async fn ingest_single_line() {
        let store = Arc::new(Store::new(1_000_000));
        let app = router(test_state(Arc::clone(&store)));
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

        let (entries, _) = store
            .query_logs(Some("api"), None, 1, &CompiledQuery::MatchAll, None, None)
            .await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].data["msg"], json!("hi"));
        let mzp = entries[0].data.get("_mzp").expect("_mzp injected");
        assert!(mzp.get("cwd").is_some());
        assert!(mzp.get("pid").is_some());
        assert!(mzp.get("user").is_some());
        assert!(mzp.get("exe").is_some());
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
            .query_logs(Some("shell"), None, 10, &CompiledQuery::MatchAll, None, None)
            .await;
        assert_eq!(entries.len(), 3);
        // Newest first
        assert_eq!(entries[0].data["_raw"], json!("plain"));
        assert_eq!(entries[1].data["n"], json!(2));
        assert_eq!(entries[2].data["n"], json!(1));
        assert!(entries.iter().all(|e| e.data.get("_mzp").is_some()));
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
            .query_logs(
                Some("/Users/me/app"),
                None,
                10,
                &CompiledQuery::MatchAll,
                None,
                None,
            )
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

    #[tokio::test]
    async fn get_update_returns_status() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/update")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["currentVersion"], env!("CARGO_PKG_VERSION"));
        assert_eq!(parsed["updateAvailable"], false);
        assert_eq!(parsed["busy"], false);
        assert!(parsed["channel"].is_string());
    }

    #[tokio::test]
    async fn post_update_rejects_non_loopback() {
        let app = test_app();
        // No ConnectInfo → PeerAddr defaults to non-loopback → 403
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/update")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn post_update_no_update_is_400() {
        use axum::extract::ConnectInfo;
        use std::net::SocketAddr;

        let app = test_app();
        let mut req = Request::builder()
            .method("POST")
            .uri("/api/update")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 9))));
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn disconnect_blocks_ingest_and_lists_blocked() {
        let store = Arc::new(Store::new(1_000_000));
        let app = router(test_state(Arc::clone(&store)));

        let resp = app
            .clone()
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

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/services/disconnect")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"service":"api"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ingest")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"service":"api","line":"{\"msg\":\"again\"}"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/services")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["services"], json!([]));
        assert_eq!(parsed["blocked"], json!(["api"]));

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/services/reconnect")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"service":"api"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ingest")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"service":"api","line":"{\"msg\":\"back\"}"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(store.stats().await.count, 1);
    }
}
