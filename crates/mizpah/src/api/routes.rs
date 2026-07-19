//! REST API handlers.

use super::AppState;
use crate::error::ApiError;
use crate::filter::compile_query;
use crate::investigate::{self, InvestigateTarget};
use crate::mzp_meta::MzpMeta;
use crate::store::{ActivityBucket, LogEntry, PropertyInfo, PushLineResult, Stats};
use crate::update::{self, ApplyBeginError, UpdateEvent};
use axum::extract::{ConnectInfo, FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderValue};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures_util::stream;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

const INGEST_BATCH_MAX: usize = 128;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IngestRequest {
    service: String,
    line: String,
    #[serde(default)]
    cmd: Option<String>,
    #[serde(default)]
    mzp: Option<MzpMeta>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IngestResponse {
    /// Entries emitted by this line (empty while buffering a pretty block).
    entries: Vec<LogEntry>,
}

pub(crate) async fn ingest(
    State(state): State<AppState>,
    Json(body): Json<IngestRequest>,
) -> Result<Json<IngestResponse>, ApiError> {
    if body.service.is_empty() {
        return Err(ApiError::bad_request("service is required"));
    }
    // Prefer client-provided receiver meta; fall back to hub process so every row has `_mzp`.
    let mzp = body.mzp.unwrap_or_else(MzpMeta::capture);
    match state
        .store
        .push_line_with_meta(&body.service, &body.line, body.cmd.as_deref(), Some(&mzp))
        .await
    {
        PushLineResult::Blocked => Err(ApiError::conflict("service disconnected")),
        PushLineResult::Emitted(entries) => Ok(Json(IngestResponse { entries })),
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IngestBatchRequest {
    service: String,
    lines: Vec<String>,
    #[serde(default)]
    cmd: Option<String>,
    #[serde(default)]
    mzp: Option<MzpMeta>,
    /// Optional locked format id for file ingest (pack or stable Mizpah id).
    #[serde(default)]
    format_hint: Option<String>,
}

pub(crate) async fn ingest_batch(
    State(state): State<AppState>,
    Json(body): Json<IngestBatchRequest>,
) -> Result<Json<IngestResponse>, ApiError> {
    if body.service.is_empty() {
        return Err(ApiError::bad_request("service is required"));
    }
    if body.lines.len() > INGEST_BATCH_MAX {
        return Err(ApiError::bad_request(format!(
            "at most {INGEST_BATCH_MAX} lines per batch"
        )));
    }
    let cmd = body.cmd.as_deref();
    let mzp = body.mzp.unwrap_or_else(MzpMeta::capture);
    let hint = body.format_hint.as_deref();
    let mut entries = Vec::new();
    for line in &body.lines {
        match state
            .store
            .push_line_with_meta_hint(&body.service, line, cmd, Some(&mzp), hint)
            .await
        {
            PushLineResult::Blocked => {
                return Err(ApiError::conflict("service disconnected"));
            }
            PushLineResult::Emitted(batch) => entries.extend(batch),
        }
    }
    Ok(Json(IngestResponse { entries }))
}

#[derive(Debug, Deserialize)]
pub(crate) struct LogsQuery {
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LogsResponse {
    entries: Vec<LogEntry>,
    has_more: bool,
}

pub(crate) fn parse_rfc3339(
    label: &str,
    value: Option<&str>,
) -> Result<Option<DateTime<Utc>>, String> {
    match value.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => DateTime::parse_from_rfc3339(s)
            .map(|dt| Some(dt.with_timezone(&Utc)))
            .map_err(|e| format!("invalid {label}: {e}")),
    }
}

pub(crate) async fn list_logs(
    State(state): State<AppState>,
    Query(params): Query<LogsQuery>,
) -> Result<Json<LogsResponse>, ApiError> {
    let query = compile_query(params.q.as_deref().unwrap_or("")).map_err(ApiError::from)?;
    let from = parse_rfc3339("from", params.from.as_deref()).map_err(ApiError::bad_request)?;
    let to = parse_rfc3339("to", params.to.as_deref()).map_err(ApiError::bad_request)?;
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
pub(crate) struct ActivityQuery {
    /// Trailing window in hours (default 24).
    hours: Option<u64>,
    /// Bucket size in minutes (default 25).
    #[serde(rename = "bucketMinutes")]
    bucket_minutes: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActivityResponse {
    buckets: Vec<ActivityBucket>,
}

pub(crate) async fn activity(
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ServicesResponse {
    services: Vec<String>,
    blocked: Vec<String>,
}

pub(crate) async fn list_services(State(state): State<AppState>) -> Json<ServicesResponse> {
    Json(ServicesResponse {
        services: state.store.service_names().await,
        blocked: state.store.blocked_names().await,
    })
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ServiceNameRequest {
    service: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DisconnectResponse {
    service: String,
    evicted: usize,
}

pub(crate) async fn disconnect_service(
    State(state): State<AppState>,
    Json(body): Json<ServiceNameRequest>,
) -> Result<Json<DisconnectResponse>, ApiError> {
    if body.service.is_empty() {
        return Err(ApiError::bad_request("service is required"));
    }
    let evicted = state.store.disconnect_service(&body.service).await;
    Ok(Json(DisconnectResponse {
        service: body.service,
        evicted: evicted.len(),
    }))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReconnectResponse {
    service: String,
    reconnected: bool,
}

pub(crate) async fn reconnect_service(
    State(state): State<AppState>,
    Json(body): Json<ServiceNameRequest>,
) -> Result<Json<ReconnectResponse>, ApiError> {
    if body.service.is_empty() {
        return Err(ApiError::bad_request("service is required"));
    }
    let reconnected = state.store.reconnect_service(&body.service).await;
    Ok(Json(ReconnectResponse {
        service: body.service,
        reconnected,
    }))
}

#[derive(Debug, Deserialize)]
pub(crate) struct PropertiesQuery {
    service: Option<String>,
    /// Case-insensitive substring match against property paths and sample values.
    q: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PropertiesResponse {
    properties: Vec<PropertyInfo>,
}

pub(crate) async fn list_properties(
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

pub(crate) async fn stats(State(state): State<AppState>) -> Json<Stats> {
    Json(state.store.stats().await)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InvestigateRequest {
    target: InvestigateTarget,
    id: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InvestigateResponse {
    ok: bool,
}

pub(crate) async fn investigate(
    State(state): State<AppState>,
    Json(body): Json<InvestigateRequest>,
) -> Result<Json<InvestigateResponse>, ApiError> {
    let entry = state
        .store
        .get_entry(body.id)
        .await
        .ok_or_else(|| ApiError::not_found(format!("log entry {} not found", body.id)))?;

    let project_dir = state.project_dir.clone();
    let target = body.target;
    tokio::task::spawn_blocking(move || investigate::launch_session(target, &entry, &project_dir))
        .await
        .map_err(|e| ApiError::internal(format!("investigate task failed: {e}")))?
        .map_err(ApiError::bad_gateway)?;

    Ok(Json(InvestigateResponse { ok: true }))
}

pub(crate) async fn get_update(State(state): State<AppState>) -> Json<update::UpdateStatus> {
    Json(state.update.status().await)
}

/// Peer address for privileged routes. Missing connect info is treated as non-loopback.
pub(crate) struct PeerAddr(SocketAddr);

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

pub(crate) async fn post_update(
    PeerAddr(peer): PeerAddr,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    if !peer.ip().is_loopback() {
        return Err(ApiError::forbidden(
            "updates are only allowed from localhost",
        ));
    }

    let latest = match state.update.try_begin_apply().await {
        Ok(v) => v,
        Err(ApplyBeginError::Busy) => {
            return Err(ApiError::conflict("an update is already in progress"));
        }
        Err(ApplyBeginError::NoUpdate) => {
            return Err(ApiError::bad_request("no update available"));
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
    let store = Arc::clone(&state.store);
    tokio::spawn(async move {
        let outcome = update::apply_update(manager, store, latest, tx).await;
        if outcome == update::ApplyOutcome::RestartRequested {
            std::process::exit(0);
        }
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
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    res.headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok(res)
}

// --- Phase B–H investigation APIs ---

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AggregateQuery {
    service: Option<String>,
    q: Option<String>,
    from: Option<String>,
    to: Option<String>,
    /// Comma-separated group-by paths
    group_by: Option<String>,
    limit: Option<usize>,
    field: Option<String>,
    sum: Option<bool>,
    avg: Option<bool>,
    min: Option<bool>,
    max: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AggregateResponse {
    rows: Vec<crate::store::AggregateRow>,
}

pub(crate) async fn aggregate(
    State(state): State<AppState>,
    Query(params): Query<AggregateQuery>,
) -> Result<Json<AggregateResponse>, ApiError> {
    let query = compile_query(params.q.as_deref().unwrap_or("")).map_err(ApiError::from)?;
    let from = parse_rfc3339("from", params.from.as_deref()).map_err(ApiError::bad_request)?;
    let to = parse_rfc3339("to", params.to.as_deref()).map_err(ApiError::bad_request)?;
    let group_by: Vec<String> = params
        .group_by
        .as_deref()
        .unwrap_or("service")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let metrics = crate::store::AggregateMetrics {
        field: params.field,
        sum: params.sum.unwrap_or(false),
        avg: params.avg.unwrap_or(false),
        min: params.min.unwrap_or(false),
        max: params.max.unwrap_or(false),
    };
    let rows = state
        .store
        .aggregate_logs(
            params.service.as_deref(),
            &query,
            from,
            to,
            &group_by,
            &metrics,
            params.limit.unwrap_or(20),
        )
        .await;
    Ok(Json(AggregateResponse { rows }))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AggregateBody {
    service: Option<String>,
    q: Option<String>,
    from: Option<String>,
    to: Option<String>,
    #[serde(default)]
    group_by: Vec<String>,
    #[serde(default)]
    metrics: crate::store::AggregateMetrics,
    limit: Option<usize>,
}

pub(crate) async fn aggregate_post(
    State(state): State<AppState>,
    Json(body): Json<AggregateBody>,
) -> Result<Json<AggregateResponse>, ApiError> {
    let query = compile_query(body.q.as_deref().unwrap_or("")).map_err(ApiError::from)?;
    let from = parse_rfc3339("from", body.from.as_deref()).map_err(ApiError::bad_request)?;
    let to = parse_rfc3339("to", body.to.as_deref()).map_err(ApiError::bad_request)?;
    let group_by = if body.group_by.is_empty() {
        vec!["service".into()]
    } else {
        body.group_by
    };
    let rows = state
        .store
        .aggregate_logs(
            body.service.as_deref(),
            &query,
            from,
            to,
            &group_by,
            &body.metrics,
            body.limit.unwrap_or(50),
        )
        .await;
    Ok(Json(AggregateResponse { rows }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NavLevelQuery {
    from_id: Option<u64>,
    direction: Option<String>,
    levels: Option<String>,
    service: Option<String>,
    q: Option<String>,
    from: Option<String>,
    to: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NavLevelResponse {
    entry: Option<LogEntry>,
}

pub(crate) async fn nav_level(
    State(state): State<AppState>,
    Query(params): Query<NavLevelQuery>,
) -> Result<Json<NavLevelResponse>, ApiError> {
    let query = compile_query(params.q.as_deref().unwrap_or("")).map_err(ApiError::from)?;
    let from = parse_rfc3339("from", params.from.as_deref()).map_err(ApiError::bad_request)?;
    let to = parse_rfc3339("to", params.to.as_deref()).map_err(ApiError::bad_request)?;
    let direction = match params
        .direction
        .as_deref()
        .unwrap_or("next")
        .to_ascii_lowercase()
        .as_str()
    {
        "prev" | "previous" => crate::store::NavDirection::Prev,
        _ => crate::store::NavDirection::Next,
    };
    let level_owned: Vec<String> = params
        .levels
        .as_deref()
        .unwrap_or("error,warn")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let level_refs: Vec<&str> = level_owned.iter().map(|s| s.as_str()).collect();
    let entry = state
        .store
        .find_level_near(
            params.from_id.unwrap_or(u64::MAX),
            direction,
            &level_refs,
            params.service.as_deref(),
            &query,
            from,
            to,
        )
        .await;
    Ok(Json(NavLevelResponse { entry }))
}

#[derive(Debug, Deserialize)]
pub(crate) struct TraceListQuery {
    limit: Option<usize>,
}

pub(crate) async fn list_traces(
    State(state): State<AppState>,
    Query(params): Query<TraceListQuery>,
) -> Json<serde_json::Value> {
    let traces = state.store.list_traces(params.limit.unwrap_or(50)).await;
    Json(serde_json::json!({ "traces": traces }))
}

pub(crate) async fn get_trace(
    State(state): State<AppState>,
    axum::extract::Path(opid): axum::extract::Path<String>,
    Query(params): Query<TraceListQuery>,
) -> Json<LogsResponse> {
    let entries = state
        .store
        .get_trace(&opid, params.limit.unwrap_or(100))
        .await;
    Json(LogsResponse {
        entries,
        has_more: false,
    })
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BookmarkBody {
    id: u64,
    marked: Option<bool>,
    tags: Option<Vec<String>>,
    comment: Option<String>,
}

pub(crate) async fn list_bookmarks(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "bookmarks": state.store.list_bookmarks().await }))
}

pub(crate) async fn set_bookmark(
    State(state): State<AppState>,
    Json(body): Json<BookmarkBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ann = state
        .store
        .set_bookmark(body.id, body.marked, body.tags, Some(body.comment))
        .await
        .ok_or_else(|| ApiError::not_found("entry not found"))?;
    Ok(Json(
        serde_json::json!({ "id": body.id, "annotation": ann }),
    ))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TagBody {
    q: String,
    tag: String,
}

pub(crate) async fn tag_logs(
    State(state): State<AppState>,
    Json(body): Json<TagBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let n = state
        .store
        .tag_by_cel(&body.q, &body.tag)
        .await
        .map_err(ApiError::bad_request)?;
    Ok(Json(serde_json::json!({ "tagged": n })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SpectrogramQuery {
    field: Option<String>,
    from: Option<String>,
    to: Option<String>,
    time_buckets: Option<usize>,
    value_buckets: Option<usize>,
}

pub(crate) async fn spectrogram(
    State(state): State<AppState>,
    Query(params): Query<SpectrogramQuery>,
) -> Result<Json<crate::store::SpectrogramResult>, ApiError> {
    let from = parse_rfc3339("from", params.from.as_deref()).map_err(ApiError::bad_request)?;
    let to = parse_rfc3339("to", params.to.as_deref()).map_err(ApiError::bad_request)?;
    let field = params.field.as_deref().unwrap_or("level");
    let result = state
        .store
        .spectrogram(
            field,
            from,
            to,
            params.time_buckets.unwrap_or(24),
            params.value_buckets.unwrap_or(10),
        )
        .await;
    Ok(Json(result))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SqlBody {
    sql: String,
    limit: Option<usize>,
}

pub(crate) async fn run_sql(
    State(state): State<AppState>,
    Json(body): Json<SqlBody>,
) -> Result<Json<crate::sql::SqlResult>, ApiError> {
    state
        .store
        .query_sql(&body.sql, body.limit.unwrap_or(100))
        .await
        .map(Json)
        .map_err(|e| ApiError::bad_request(e.to_string()))
}

pub(crate) async fn get_keymap() -> Json<crate::keymap::Keymap> {
    Json(crate::keymap::Keymap::load())
}

#[derive(Debug, Deserialize)]
pub(crate) struct ThemeQuery {
    name: Option<String>,
}

pub(crate) async fn get_themes(Query(params): Query<ThemeQuery>) -> Json<serde_json::Value> {
    let _ = crate::keymap::themes::ensure_default_themes();
    let names = crate::keymap::themes::list_theme_names();
    if let Some(name) = params.name.as_deref() {
        let theme = crate::keymap::themes::load_theme(name);
        return Json(serde_json::json!({ "themes": names, "theme": theme }));
    }
    Json(serde_json::json!({
        "themes": names,
        "theme": crate::keymap::themes::default_theme(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::spawn_test_hub;

    #[tokio::test]
    async fn parse_rfc3339_valid() {
        let dt = parse_rfc3339("test", Some("2024-01-01T00:00:00Z")).unwrap();
        assert!(dt.is_some());
    }

    #[tokio::test]
    async fn parse_rfc3339_empty() {
        let dt = parse_rfc3339("test", Some("  ")).unwrap();
        assert!(dt.is_none());
    }

    #[tokio::test]
    async fn parse_rfc3339_invalid() {
        let result = parse_rfc3339("test", Some("not-a-date"));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn parse_rfc3339_none() {
        let dt = parse_rfc3339("test", None).unwrap();
        assert!(dt.is_none());
    }

    #[tokio::test]
    async fn peer_addr_from_connect_info() {
        use axum::extract::FromRequestParts;
        use axum::http::Request;
        let mut req = Request::builder().body(()).unwrap();
        let addr: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(addr));
        let (mut parts, _) = req.into_parts();
        let result = PeerAddr::from_request_parts(&mut parts, &()).await;
        assert!(result.is_ok());
        let peer = result.unwrap();
        assert_eq!(peer.0, addr);
    }

    #[tokio::test]
    async fn peer_addr_missing_connect_info() {
        use axum::extract::FromRequestParts;
        use axum::http::Request;
        let req = Request::builder().body(()).unwrap();
        let (mut parts, _) = req.into_parts();
        let result = PeerAddr::from_request_parts(&mut parts, &()).await;
        assert!(result.is_ok());
        let peer = result.unwrap();
        assert!(!peer.0.ip().is_loopback());
    }

    #[tokio::test]
    async fn ingest_response_serialization() {
        let resp = IngestResponse { entries: vec![] };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("entries"));
    }

    #[tokio::test]
    async fn logs_response_serialization() {
        let resp = LogsResponse {
            entries: vec![],
            has_more: false,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("entries"));
        assert!(json.contains("hasMore"));
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn ingest_batch_route_with_format_hint() {
        let (url, store) = spawn_test_hub().await;
        let client = reqwest::Client::new();
        let req = IngestBatchRequest {
            service: "api".into(),
            lines: vec![r#"{"msg":"a"}"#.into(), r#"{"msg":"b"}"#.into()],
            cmd: Some("test-cmd".into()),
            mzp: Some(crate::mzp_meta::MzpMeta::capture()),
            format_hint: Some("json".into()),
        };
        let resp = client
            .post(format!("{url}/api/ingest/batch"))
            .json(&req)
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let entries = store.snapshot_entries().await;
        assert!(entries.len() >= 2);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn ingest_batch_route_rejects_oversized() {
        let (url, _store) = spawn_test_hub().await;
        let client = reqwest::Client::new();
        let lines: Vec<String> = (0..200)
            .map(|i| format!("{{\"msg\":\"line{i}\"}}"))
            .collect();
        let req = IngestBatchRequest {
            service: "api".into(),
            lines,
            cmd: None,
            mzp: None,
            format_hint: None,
        };
        let resp = client
            .post(format!("{url}/api/ingest/batch"))
            .json(&req)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn disconnect_service_route() {
        let (url, store) = spawn_test_hub().await;
        store.push_line("api", r#"{"msg":"test"}"#).await;
        let client = reqwest::Client::new();
        let req = ServiceNameRequest {
            service: "api".into(),
        };
        let resp = client
            .post(format!("{url}/api/services/disconnect"))
            .json(&req)
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body: DisconnectResponse = resp.json().await.unwrap();
        assert_eq!(body.service, "api");
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn reconnect_service_route() {
        let (url, store) = spawn_test_hub().await;
        store.disconnect_service("api").await;
        let client = reqwest::Client::new();
        let req = ServiceNameRequest {
            service: "api".into(),
        };
        let resp = client
            .post(format!("{url}/api/services/reconnect"))
            .json(&req)
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body: ReconnectResponse = resp.json().await.unwrap();
        assert_eq!(body.service, "api");
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn aggregate_post_route() {
        let (url, store) = spawn_test_hub().await;
        store
            .push_line("api", r#"{"level":"error","msg":"a"}"#)
            .await;
        store
            .push_line("api", r#"{"level":"info","msg":"b"}"#)
            .await;
        let client = reqwest::Client::new();
        let req = AggregateBody {
            service: None,
            q: None,
            from: None,
            to: None,
            group_by: vec!["level".into()],
            metrics: crate::store::AggregateMetrics {
                field: None,
                sum: false,
                avg: false,
                min: false,
                max: false,
            },
            limit: Some(10),
        };
        let resp = client
            .post(format!("{url}/api/aggregate"))
            .json(&req)
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body: AggregateResponse = resp.json().await.unwrap();
        assert!(!body.rows.is_empty());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn set_bookmark_route() {
        let (url, store) = spawn_test_hub().await;
        store.push_line("api", r#"{"msg":"test"}"#).await;
        let entries = store.snapshot_entries().await;
        let id = entries[0].id;
        let client = reqwest::Client::new();
        let req = BookmarkBody {
            id,
            marked: Some(true),
            tags: Some(vec!["important".into()]),
            comment: Some("key log".into()),
        };
        let resp = client
            .post(format!("{url}/api/bookmarks"))
            .json(&req)
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn tag_logs_route() {
        let (url, store) = spawn_test_hub().await;
        store
            .push_line("api", r#"{"level":"error","msg":"test"}"#)
            .await;
        let client = reqwest::Client::new();
        let req = TagBody {
            q: r#"level == "error""#.into(),
            tag: "bug".into(),
        };
        let resp = client
            .post(format!("{url}/api/tags"))
            .json(&req)
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn run_sql_route() {
        let (url, store) = spawn_test_hub().await;
        store.push_line("api", r#"{"msg":"test"}"#).await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let client = reqwest::Client::new();
        let req = SqlBody {
            sql: "SELECT service FROM all_logs LIMIT 1".into(),
            limit: Some(10),
        };
        let resp = client
            .post(format!("{url}/api/sql"))
            .json(&req)
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success(), "status: {}", resp.status());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn get_keymap_route() {
        let (url, _store) = spawn_test_hub().await;
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{url}/api/keymap"))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body: crate::keymap::Keymap = resp.json().await.unwrap();
        assert!(!body.quit.is_empty());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn get_themes_route() {
        let (url, _store) = spawn_test_hub().await;
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{url}/api/themes"))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body.get("themes").is_some());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn get_themes_route_with_name() {
        let (url, _store) = spawn_test_hub().await;
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{url}/api/themes?name=default"))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body.get("theme").is_some());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn ingest_route_single_line() {
        let (url, store) = spawn_test_hub().await;
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{url}/api/ingest"))
            .json(&serde_json::json!({
                "service": "api",
                "line": r#"{"msg":"single"}"#
            }))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(store.stats().await.count >= 1);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn ingest_route_rejects_empty_service() {
        let (url, _store) = spawn_test_hub().await;
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{url}/api/ingest"))
            .json(&serde_json::json!({"service": "", "line": "{}"}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn ingest_batch_blocked_service() {
        let (url, store) = spawn_test_hub().await;
        store.disconnect_service("blocked").await;
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{url}/api/ingest/batch"))
            .json(&IngestBatchRequest {
                service: "blocked".into(),
                lines: vec![r#"{"msg":"x"}"#.into()],
                cmd: None,
                mzp: None,
                format_hint: None,
            })
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 409);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn list_logs_and_stats_routes() {
        let (url, store) = spawn_test_hub().await;
        store.push_line("api", r#"{"msg":"listed"}"#).await;
        let client = reqwest::Client::new();
        let logs = client
            .get(format!("{url}/api/logs?service=api&limit=10"))
            .send()
            .await
            .unwrap();
        assert!(logs.status().is_success());
        let body: serde_json::Value = logs.json().await.unwrap();
        assert!(body
            .get("entries")
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty()));
        let stats = client.get(format!("{url}/api/stats")).send().await.unwrap();
        assert!(stats.status().is_success());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn activity_and_services_routes() {
        let (url, store) = spawn_test_hub().await;
        store.push_line("svc", r#"{"msg":"a"}"#).await;
        let client = reqwest::Client::new();
        let activity = client
            .get(format!("{url}/api/activity?hours=1&bucketMinutes=5"))
            .send()
            .await
            .unwrap();
        assert!(activity.status().is_success());
        let services = client
            .get(format!("{url}/api/services"))
            .send()
            .await
            .unwrap();
        assert!(services.status().is_success());
        let body: serde_json::Value = services.json().await.unwrap();
        assert!(body
            .get("services")
            .and_then(|v| v.as_array())
            .is_some_and(|a| a.iter().any(|s| s.as_str() == Some("svc"))));
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn aggregate_get_and_nav_level_routes() {
        let (url, store) = spawn_test_hub().await;
        store
            .push_line("api", r#"{"level":"error","msg":"e"}"#)
            .await;
        let client = reqwest::Client::new();
        let agg = client
            .get(format!("{url}/api/aggregate?group_by=level&limit=5"))
            .send()
            .await
            .unwrap();
        assert!(agg.status().is_success());
        let nav = client
            .get(format!("{url}/api/nav/level?direction=next&levels=error"))
            .send()
            .await
            .unwrap();
        assert!(nav.status().is_success());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn traces_bookmarks_spectrogram_routes() {
        let (url, store) = spawn_test_hub().await;
        store
            .push_line("api", r#"{"level":"error","msg":"x","trace_id":"t1"}"#)
            .await;
        let client = reqwest::Client::new();
        let traces = client
            .get(format!("{url}/api/traces?limit=5"))
            .send()
            .await
            .unwrap();
        assert!(traces.status().is_success());
        let bookmarks = client
            .get(format!("{url}/api/bookmarks"))
            .send()
            .await
            .unwrap();
        assert!(bookmarks.status().is_success());
        let spec = client
            .get(format!("{url}/api/spectrogram?field=level"))
            .send()
            .await
            .unwrap();
        assert!(spec.status().is_success());
        let trace = client
            .get(format!("{url}/api/traces/t1?limit=5"))
            .send()
            .await
            .unwrap();
        assert!(trace.status().is_success());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn post_update_rejects_non_loopback_peer() {
        use crate::api::AppState;
        use crate::update;
        use std::sync::Arc;

        let store = Arc::new(crate::store::Store::new(1024));
        let state = AppState {
            store: Arc::clone(&store),
            project_dir: std::env::temp_dir(),
            update: update::UpdateManager::new(update::RestartContext {
                host: "127.0.0.1".into(),
                port: 3149,
                project_dir: std::env::temp_dir(),
                max_bytes: 1024,
                ttl_hours: 0,
            }),
        };
        let peer = PeerAddr("203.0.113.1:0".parse().unwrap());
        let result = post_update(peer, State(state)).await;
        assert!(matches!(result, Err(ApiError::Forbidden(_))));
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn disconnect_and_reconnect_empty_service_rejected() {
        let (url, _store) = spawn_test_hub().await;
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{url}/api/services/disconnect"))
            .json(&ServiceNameRequest {
                service: String::new(),
            })
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
    }
}
