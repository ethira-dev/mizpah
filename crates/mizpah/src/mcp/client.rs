//! HTTP client for the Mizpah hub REST API.

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const DEFAULT_LIMIT: usize = 20;
pub const MAX_LIMIT: usize = 50;

#[derive(Debug, Error)]
pub enum HubError {
    #[error("Mizpah hub is not reachable at {url}. Start a hub first, e.g. `my-app | mizpah`")]
    Unreachable { url: String, source: reqwest::Error },
    #[error("hub request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("hub returned {status}: {body}")]
    Http { status: u16, body: String },
}

#[derive(Debug, Clone)]
pub struct HubClient {
    http: Client,
    base_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogsResponse {
    pub entries: Vec<Value>,
    pub has_more: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServicesResponse {
    pub services: Vec<String>,
    #[serde(default)]
    pub blocked: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PropertiesResponse {
    pub properties: Vec<Value>,
}

impl HubClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let base = base_url.into().trim_end_matches('/').to_string();
        Self {
            http: Client::new(),
            base_url: base,
        }
    }

    pub fn clamp_limit(limit: Option<usize>) -> usize {
        limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, HubError> {
        let url = format!("{}{path}", self.base_url);
        let response = self.http.get(&url).query(query).send().await.map_err(|e| {
            if e.is_connect() || e.is_timeout() {
                HubError::Unreachable {
                    url: self.base_url.clone(),
                    source: e,
                }
            } else {
                HubError::Request(e)
            }
        })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(HubError::Http {
                status: status.as_u16(),
                body,
            });
        }

        response.json().await.map_err(HubError::Request)
    }

    pub async fn list_services(&self) -> Result<ServicesResponse, HubError> {
        self.get_json("/api/services", &[]).await
    }

    pub async fn get_stats(&self) -> Result<Value, HubError> {
        self.get_json("/api/stats", &[]).await
    }

    pub async fn list_properties(
        &self,
        service: Option<&str>,
        q: Option<&str>,
    ) -> Result<PropertiesResponse, HubError> {
        let mut query = Vec::new();
        if let Some(svc) = service.filter(|s| !s.is_empty()) {
            query.push(("service", svc.to_string()));
        }
        if let Some(expr) = q.filter(|s| !s.is_empty()) {
            query.push(("q", expr.to_string()));
        }
        self.get_json("/api/properties", &query).await
    }

    pub async fn search_logs(
        &self,
        service: Option<&str>,
        q: Option<&str>,
        limit: Option<usize>,
        cursor: Option<u64>,
    ) -> Result<LogsResponse, HubError> {
        let mut query = Vec::new();
        if let Some(svc) = service.filter(|s| !s.is_empty()) {
            query.push(("service", svc.to_string()));
        }
        if let Some(expr) = q.filter(|s| !s.is_empty()) {
            query.push(("q", expr.to_string()));
        }
        query.push(("limit", Self::clamp_limit(limit).to_string()));
        if let Some(c) = cursor {
            query.push(("cursor", c.to_string()));
        }
        self.get_json("/api/logs", &query).await
    }

    /// Fetch a window of logs around `id` (older = before, newer = after).
    pub async fn get_logs_around(
        &self,
        id: u64,
        before: usize,
        after: usize,
        service: Option<&str>,
        q: Option<&str>,
    ) -> Result<LogsResponse, HubError> {
        let before = before.min(MAX_LIMIT);
        let after = after.min(MAX_LIMIT);
        let limit = (before + after + 1).clamp(1, MAX_LIMIT);
        let cursor = id.saturating_add(after as u64).saturating_add(1);

        let mut response = self
            .search_logs(service, q, Some(limit), Some(cursor))
            .await?;

        let min_id = id.saturating_sub(before as u64);
        let max_id = id.saturating_add(after as u64);
        response.entries.retain(|entry| {
            entry
                .get("id")
                .and_then(|v| v.as_u64())
                .map(|eid| eid >= min_id && eid <= max_id)
                .unwrap_or(false)
        });
        response
            .entries
            .sort_by_key(|entry| entry.get("id").and_then(|v| v.as_u64()).unwrap_or(0));
        response.has_more = false;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{self, AppState};
    use crate::store::Store;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::net::TcpListener;

    #[test]
    fn clamp_limit_defaults_and_caps() {
        assert_eq!(HubClient::clamp_limit(None), DEFAULT_LIMIT);
        assert_eq!(HubClient::clamp_limit(Some(0)), 1);
        assert_eq!(HubClient::clamp_limit(Some(1000)), MAX_LIMIT);
        assert_eq!(HubClient::clamp_limit(Some(10)), 10);
    }

    async fn spawn_test_hub() -> (String, Arc<Store>) {
        let store = Arc::new(Store::new(1024 * 1024));
        let state = AppState {
            store: Arc::clone(&store),
            project_dir: std::env::temp_dir(),
        };
        let app = api::router(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), store)
    }

    #[tokio::test]
    async fn search_logs_against_hub() {
        let (url, store) = spawn_test_hub().await;
        store
            .push_line("api", r#"{"level":"error","msg":"boom"}"#)
            .await;
        store
            .push_line("api", r#"{"level":"info","msg":"ok"}"#)
            .await;

        let client = HubClient::new(url);
        let resp = client
            .search_logs(Some("api"), Some(r#"level == "error""#), Some(10), None)
            .await
            .unwrap();
        assert_eq!(resp.entries.len(), 1);
        assert_eq!(resp.entries[0]["data"]["msg"].as_str(), Some("boom"));
    }

    #[tokio::test]
    async fn hub_unreachable_is_clear_error() {
        let client = HubClient::new("http://127.0.0.1:1");
        let err = client.list_services().await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not reachable") || msg.contains("hub request failed"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn get_logs_around_window() {
        let (url, store) = spawn_test_hub().await;
        for i in 0..10 {
            store.push_line("api", &format!(r#"{{"n":{i}}}"#)).await;
        }
        let client = HubClient::new(url);
        let all = client
            .search_logs(None, None, Some(50), None)
            .await
            .unwrap();
        let mid = all.entries[5]["id"].as_u64().unwrap();
        let window = client.get_logs_around(mid, 2, 2, None, None).await.unwrap();
        assert!(!window.entries.is_empty());
        let ids: Vec<u64> = window
            .entries
            .iter()
            .filter_map(|e| e["id"].as_u64())
            .collect();
        assert!(ids.contains(&mid));
        assert!(ids.iter().all(|&id| id >= mid - 2 && id <= mid + 2));
    }
}
