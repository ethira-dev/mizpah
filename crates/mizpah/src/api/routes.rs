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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IngestBatchRequest {
    service: String,
    lines: Vec<String>,
    #[serde(default)]
    cmd: Option<String>,
    #[serde(default)]
    mzp: Option<MzpMeta>,
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
    let mut entries = Vec::new();
    for line in &body.lines {
        match state
            .store
            .push_line_with_meta(&body.service, line, cmd, Some(&mzp))
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

#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ServiceNameRequest {
    service: String,
}

#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
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
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    res.headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok(res)
}
