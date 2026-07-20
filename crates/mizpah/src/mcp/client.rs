//! HTTP client for the Mizpah hub REST API.

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const DEFAULT_LIMIT: usize = 20;
pub const MAX_LIMIT: usize = 50;

fn urlencoding_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[derive(Debug, Error)]
pub enum HubClientError {
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
        crate::util::ensure_rustls_crypto_provider();
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
    ) -> Result<T, HubClientError> {
        let url = format!("{}{path}", self.base_url);
        let response = self.http.get(&url).query(query).send().await.map_err(|e| {
            if e.is_connect() || e.is_timeout() {
                HubClientError::Unreachable {
                    url: self.base_url.clone(),
                    source: e,
                }
            } else {
                HubClientError::Request(e)
            }
        })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(HubClientError::Http {
                status: status.as_u16(),
                body,
            });
        }

        response.json().await.map_err(HubClientError::Request)
    }

    pub async fn list_services(&self) -> Result<ServicesResponse, HubClientError> {
        self.get_json("/api/services", &[]).await
    }

    pub async fn get_stats(&self) -> Result<Value, HubClientError> {
        self.get_json("/api/stats", &[]).await
    }

    /// Incident / "what broke?" summary for the last `minutes`.
    pub async fn get_incident(&self, minutes: u64) -> Result<Value, HubClientError> {
        self.get_json("/api/incident", &[("minutes", minutes.max(1).to_string())])
            .await
    }

    pub async fn list_properties(
        &self,
        service: Option<&str>,
        q: Option<&str>,
    ) -> Result<PropertiesResponse, HubClientError> {
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
    ) -> Result<LogsResponse, HubClientError> {
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

    pub async fn aggregate_logs(
        &self,
        service: Option<&str>,
        q: Option<&str>,
        group_by: &[String],
        limit: Option<usize>,
    ) -> Result<Value, HubClientError> {
        let mut query = Vec::new();
        if let Some(svc) = service.filter(|s| !s.is_empty()) {
            query.push(("service", svc.to_string()));
        }
        if let Some(expr) = q.filter(|s| !s.is_empty()) {
            query.push(("q", expr.to_string()));
        }
        if !group_by.is_empty() {
            query.push(("groupBy", group_by.join(",")));
        }
        query.push(("limit", Self::clamp_limit(limit).to_string()));
        self.get_json("/api/aggregate", &query).await
    }

    pub async fn get_trace(
        &self,
        opid: &str,
        limit: Option<usize>,
    ) -> Result<LogsResponse, HubClientError> {
        let mut query = Vec::new();
        query.push(("limit", Self::clamp_limit(limit).to_string()));
        let path = format!("/api/trace/{}", urlencoding_encode(opid));
        self.get_json(&path, &query).await
    }

    pub async fn query_sql(&self, sql: &str, limit: Option<usize>) -> Result<Value, HubClientError> {
        let url = format!("{}/api/sql", self.base_url);
        let body = serde_json::json!({
            "sql": sql,
            "limit": Self::clamp_limit(limit),
        });
        let response = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() || e.is_timeout() {
                    HubClientError::Unreachable {
                        url: self.base_url.clone(),
                        source: e,
                    }
                } else {
                    HubClientError::Request(e)
                }
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(HubClientError::Http {
                status: status.as_u16(),
                body,
            });
        }
        response.json().await.map_err(HubClientError::Request)
    }

    /// Fetch a window of logs around `id` (older = before, newer = after).
    pub async fn get_logs_around(
        &self,
        id: u64,
        before: usize,
        after: usize,
        service: Option<&str>,
        q: Option<&str>,
    ) -> Result<LogsResponse, HubClientError> {
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
                .is_some_and(|eid| eid >= min_id && eid <= max_id)
        });
        response
            .entries
            .sort_by_key(|entry| entry.get("id").and_then(|v| v.as_u64()).unwrap_or(0));
        response.has_more = false;
        Ok(response)
    }

    pub async fn nav_level(
        &self,
        from_id: u64,
        direction: &str,
        levels: &[&str],
    ) -> Result<Option<Value>, HubClientError> {
        let levels_joined = levels.join(",");
        let query = [
            ("fromId", from_id.to_string()),
            ("direction", direction.to_string()),
            ("levels", levels_joined),
        ];
        let query_refs: Vec<(&str, String)> = query.into_iter().collect();
        let resp: Value = self.get_json("/api/nav/level", &query_refs).await?;
        Ok(resp.get("entry").cloned().filter(|v| !v.is_null()))
    }

    pub async fn list_bookmarks(&self) -> Result<Value, HubClientError> {
        self.get_json("/api/bookmarks", &[]).await
    }

    pub async fn list_traces(&self, limit: Option<usize>) -> Result<Value, HubClientError> {
        let query = [("limit", Self::clamp_limit(limit).to_string())];
        self.get_json("/api/traces", &query).await
    }

    pub async fn spectrogram(
        &self,
        field: &str,
        time_buckets: Option<usize>,
    ) -> Result<Value, HubClientError> {
        let mut query = vec![("field", field.to_string())];
        if let Some(n) = time_buckets {
            query.push(("timeBuckets", n.to_string()));
        }
        self.get_json("/api/spectrogram", &query).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_limit_defaults_and_caps() {
        assert_eq!(HubClient::clamp_limit(None), DEFAULT_LIMIT);
        assert_eq!(HubClient::clamp_limit(Some(0)), 1);
        assert_eq!(HubClient::clamp_limit(Some(1000)), MAX_LIMIT);
        assert_eq!(HubClient::clamp_limit(Some(10)), 10);
    }

    // Real TCP / reqwest sockets are unsupported under Miri.
    #[cfg(not(miri))]
    use crate::test_support::hub::spawn_test_hub;

    #[cfg(not(miri))]
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

    #[cfg(not(miri))]
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

    #[cfg(not(miri))]
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

    #[cfg(not(miri))]
    #[tokio::test]
    async fn get_logs_around_clamps_window() {
        let (url, store) = spawn_test_hub().await;
        store.push_line("api", r#"{"msg":"test"}"#).await;
        let client = HubClient::new(url);
        let all = client.search_logs(None, None, Some(1), None).await.unwrap();
        let id = all.entries[0]["id"].as_u64().unwrap();
        
        // Request huge window - should be clamped
        let window = client.get_logs_around(id, 1000, 1000, None, None).await.unwrap();
        assert!(!window.entries.is_empty());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn aggregate_logs_with_filters() {
        let (url, store) = spawn_test_hub().await;
        store.push_line("api", r#"{"level":"error"}"#).await;
        store.push_line("backend", r#"{"level":"info"}"#).await;

        let client = HubClient::new(url);
        let resp = client
            .aggregate_logs(Some("api"), Some(r#"level == "error""#), &["level".into()], Some(10))
            .await
            .unwrap();
        assert!(resp.is_object());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn nav_level_prev() {
        let (url, store) = spawn_test_hub().await;
        store.push_line("api", r#"{"level":"error","msg":"1"}"#).await;
        store.push_line("api", r#"{"level":"info","msg":"2"}"#).await;
        store.push_line("api", r#"{"level":"error","msg":"3"}"#).await;

        let client = HubClient::new(url);
        let all = client.search_logs(None, None, Some(50), None).await.unwrap();
        let last_id = all.entries[0]["id"].as_u64().unwrap();

        let result = client
            .nav_level(last_id, "prev", &["error"])
            .await
            .unwrap();
        assert!(result.is_some());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn list_traces_with_limit() {
        let (url, store) = spawn_test_hub().await;
        store.push_line("api", r#"{"opid":"req-1","msg":"test"}"#).await;
        store.push_line("api", r#"{"opid":"req-2","msg":"test"}"#).await;

        let client = HubClient::new(url);
        let resp = client.list_traces(Some(1)).await.unwrap();
        assert!(resp.is_object() || resp.is_array());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn spectrogram_with_buckets() {
        let (url, store) = spawn_test_hub().await;
        store.push_line("api", r#"{"level":"error"}"#).await;

        let client = HubClient::new(url);
        let resp = client.spectrogram("level", Some(5)).await.unwrap();
        assert!(resp.is_object() || resp.is_array());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn get_trace_with_limit() {
        let (url, store) = spawn_test_hub().await;
        store.push_line("api", r#"{"opid":"req-123","msg":"1"}"#).await;
        store.push_line("api", r#"{"opid":"req-123","msg":"2"}"#).await;

        let client = HubClient::new(url);
        let resp = client.get_trace("req-123", Some(1)).await.unwrap();
        assert!(!resp.entries.is_empty());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn list_properties_with_search() {
        let (url, store) = spawn_test_hub().await;
        store.push_line("api", r#"{"level":"error","msg":"boom"}"#).await;

        let client = HubClient::new(url);
        let resp = client.list_properties(Some("api"), Some("level")).await.unwrap();
        assert!(!resp.properties.is_empty());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn search_logs_with_cursor() {
        let (url, store) = spawn_test_hub().await;
        for i in 0..5 {
            store.push_line("api", &format!(r#"{{"n":{i}}}"#)).await;
        }

        let client = HubClient::new(url);
        let page1 = client.search_logs(None, None, Some(2), None).await.unwrap();
        assert_eq!(page1.entries.len(), 2);
        
        let cursor = page1.entries.last().unwrap()["id"].as_u64().unwrap();
        let page2 = client.search_logs(None, None, Some(2), Some(cursor)).await.unwrap();
        assert!(!page2.entries.is_empty());
    }

    #[test]
    fn urlencoding_basic() {
        assert_eq!(urlencoding_encode("hello"), "hello");
        assert_eq!(urlencoding_encode("hello world"), "hello%20world");
        assert_eq!(urlencoding_encode("a/b"), "a%2Fb");
        assert_eq!(urlencoding_encode("a+b"), "a%2Bb");
        assert_eq!(urlencoding_encode("a=b&c=d"), "a%3Db%26c%3Dd");
    }

    #[test]
    fn urlencoding_preserves_unreserved() {
        assert_eq!(urlencoding_encode("abc-._~123"), "abc-._~123");
    }
}
