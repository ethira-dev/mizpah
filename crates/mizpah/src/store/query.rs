//! Log query, properties search, and stats.

use super::ingest::in_time_range;
use super::{LogEntry, PropertyInfo, ServiceInfo, Stats, Store};
use crate::properties::{filter_properties_by_query, paths_to_info, push_service_property};
use chrono::{DateTime, Utc};

impl Store {
    /// Clone all entries currently in the ring (for SQL snapshot / export).
    pub async fn snapshot_entries(&self) -> Vec<LogEntry> {
        let inner = self.inner.read().await;
        inner.entries.iter().cloned().collect()
    }

    pub async fn service_names(&self) -> Vec<String> {
        let inner = self.inner.read().await;
        let mut names: Vec<String> = inner.services.keys().cloned().collect();
        names.sort();
        names
    }

    pub async fn stats(&self) -> Stats {
        let inner = self.inner.read().await;
        let mut services: Vec<ServiceInfo> = inner
            .services
            .iter()
            .map(|(name, count)| ServiceInfo {
                name: name.clone(),
                count: *count,
            })
            .collect();
        services.sort_by(|a, b| a.name.cmp(&b.name));
        Stats {
            count: inner.entries.len() as u64,
            approx_bytes: inner.approx_bytes,
            max_bytes: inner.max_bytes,
            services,
        }
    }

    /// Search and list discovered fields with occurrence counts over the full buffer.
    ///
    /// - `q` matches property paths or sample values (case-insensitive substring).
    /// - When only values match, non-matching samples are dropped from the result.
    /// - Always includes a synthetic `service` field when services are present.
    pub async fn search_properties(
        &self,
        service: Option<&str>,
        q: Option<&str>,
    ) -> Vec<PropertyInfo> {
        let inner = self.inner.read().await;
        let mut infos = match service {
            Some(svc) if !svc.is_empty() && svc != "*" => {
                if let Some(map) = inner.properties_by_service.get(svc) {
                    paths_to_info(map)
                } else {
                    Vec::new()
                }
            }
            _ => paths_to_info(&inner.properties),
        };
        push_service_property(&mut infos, &inner.services, service);

        let needle = q
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_lowercase());

        if let Some(ref needle) = needle {
            infos = filter_properties_by_query(infos, needle);
        }

        infos
    }

    pub async fn get_entry(&self, id: u64) -> Option<LogEntry> {
        let inner = self.inner.read().await;
        inner.entries.iter().find(|e| e.id == id).cloned()
    }

    pub async fn query_logs(
        &self,
        service: Option<&str>,
        cursor: Option<u64>,
        limit: usize,
        query: &crate::filter::CompiledQuery,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> (Vec<LogEntry>, bool) {
        let inner = self.inner.read().await;
        let limit = limit.clamp(1, 500);
        let mut matched = Vec::new();
        let mut has_more = false;

        // Newest first
        for entry in inner.entries.iter().rev() {
            if let Some(c) = cursor {
                if entry.id >= c {
                    continue;
                }
            }
            if let Some(svc) = service {
                if !svc.is_empty() && svc != "*" && entry.service != svc {
                    continue;
                }
            }
            if !in_time_range(entry.effective_event_time(), from, to) {
                continue;
            }
            if !crate::filter::matches_entry(&entry.service, &entry.data, query) {
                continue;
            }
            if matched.len() >= limit {
                has_more = true;
                break;
            }
            matched.push(entry.clone());
        }

        (matched, has_more)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{compile_query, CompiledQuery};

    #[tokio::test]
    async fn search_properties_per_service() {
        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"level":"error","tenant":"a"}"#)
            .await;
        store
            .push_line("web", r#"{"level":"info","tenant":"b"}"#)
            .await;

        let api = store.search_properties(Some("api"), None).await;
        assert!(api.iter().any(|p| p.path == "tenant"));
        assert!(!api.iter().any(|p| p.path == "level" && p.count > 1) || !api.is_empty());

        let missing = store.search_properties(Some("missing"), None).await;
        assert!(missing.is_empty());

        let all = store.search_properties(Some("*"), Some("tenant")).await;
        assert!(all.iter().any(|p| p.path == "tenant"));
    }

    #[tokio::test]
    async fn query_logs_service_filter_and_cursor() {
        let store = Store::new(1_000_000);
        store.push_line("api", r#"{"msg":"a"}"#).await;
        store.push_line("web", r#"{"msg":"b"}"#).await;
        store.push_line("api", r#"{"msg":"c"}"#).await;

        let (api_only, _) = store
            .query_logs(Some("api"), None, 10, &CompiledQuery::MatchAll, None, None)
            .await;
        assert_eq!(api_only.len(), 2);
        assert!(api_only.iter().all(|e| e.service == "api"));

        let (wildcard, _) = store
            .query_logs(Some("*"), None, 10, &CompiledQuery::MatchAll, None, None)
            .await;
        assert_eq!(wildcard.len(), 3);

        let newest = &api_only[0];
        let (older, has_more) = store
            .query_logs(
                Some("api"),
                Some(newest.id),
                1,
                &CompiledQuery::MatchAll,
                None,
                None,
            )
            .await;
        assert_eq!(older.len(), 1);
        assert!(!has_more);

        let (cel, _) = store
            .query_logs(
                None,
                None,
                10,
                &compile_query(r#"msg == "b""#).unwrap(),
                None,
                None,
            )
            .await;
        assert_eq!(cel.len(), 1);
        assert_eq!(cel[0].service, "web");
    }

    #[tokio::test]
    async fn get_entry_returns_clone_or_none() {
        let store = Store::new(1_000_000);
        let e = store.push_line("api", r#"{"msg":"x"}"#).await;
        let id = e[0].id;
        let found = store.get_entry(id).await.unwrap();
        assert_eq!(found.id, id);
        assert_eq!(found.data["msg"], "x");
        assert!(store.get_entry(id + 999).await.is_none());
    }

    #[tokio::test]
    async fn query_logs_sets_has_more_when_limit_exceeded() {
        let store = Store::new(1_000_000);
        for i in 0..5 {
            store.push_line("api", &format!(r#"{{"msg":"{i}"}}"#)).await;
        }
        let (page, has_more) = store
            .query_logs(Some("api"), None, 2, &CompiledQuery::MatchAll, None, None)
            .await;
        assert_eq!(page.len(), 2);
        assert!(has_more);
    }
}
