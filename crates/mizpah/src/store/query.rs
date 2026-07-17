//! Log query, properties search, and stats.

use super::ingest::in_time_range;
use super::{LogEntry, PropertyInfo, ServiceInfo, Stats, Store};
use crate::properties::{filter_properties_by_query, paths_to_info, push_service_property};
use chrono::{DateTime, Utc};

impl Store {
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
            if !in_time_range(entry.received_at, from, to) {
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
