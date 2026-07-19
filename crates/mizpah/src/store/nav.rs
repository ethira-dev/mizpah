//! Jump to nearest matching severity relative to an entry id.

use super::ingest::in_time_range;
use super::{LogEntry, Store};
use crate::filter::CompiledQuery;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NavDirection {
    Next,
    Prev,
}

fn level_of(data: &serde_json::Value) -> String {
    data.get("level")
        .or_else(|| data.get("severity"))
        .or_else(|| data.get("lvl"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn level_matches(level: &str, wanted: &[&str]) -> bool {
    if wanted.is_empty() {
        return true;
    }
    wanted.iter().any(|w| {
        let w = w.to_ascii_lowercase();
        level == w
            || (w == "error" && matches!(level, "error" | "err" | "fatal" | "critical"))
            || (w == "warn" && matches!(level, "warn" | "warning"))
    })
}

impl Store {
    /// Find the nearest entry with a matching level in `direction` from `from_id`.
    ///
    /// - `Next`: newer than `from_id` (higher id), closest first
    /// - `Prev`: older than `from_id` (lower id), closest first
    pub async fn find_level_near(
        &self,
        from_id: u64,
        direction: NavDirection,
        levels: &[&str],
        service: Option<&str>,
        query: &CompiledQuery,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Option<LogEntry> {
        let inner = self.inner.read().await;
        let iter: Box<dyn Iterator<Item = &LogEntry> + '_> = match direction {
            NavDirection::Next => Box::new(inner.entries.iter().filter(|e| e.id > from_id)),
            NavDirection::Prev => Box::new(inner.entries.iter().rev().filter(|e| e.id < from_id)),
        };

        for entry in iter {
            if let Some(svc) = service {
                if !svc.is_empty() && svc != "*" && entry.service != svc {
                    continue;
                }
            }
            if !in_time_range(entry.effective_event_time(), from, to) {
                continue;
            }
            if !level_matches(&level_of(&entry.data), levels) {
                continue;
            }
            if !crate::filter::matches_entry(&entry.service, &entry.data, query) {
                continue;
            }
            return Some(entry.clone());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{compile_query, CompiledQuery};
    use crate::store::Store;

    #[tokio::test]
    async fn finds_next_and_prev_error() {
        let store = Store::new(1_000_000);
        store.push_line("api", r#"{"level":"info","msg":"a"}"#).await;
        let e1 = store
            .push_line("api", r#"{"level":"error","msg":"b"}"#)
            .await;
        store.push_line("api", r#"{"level":"info","msg":"c"}"#).await;
        let e2 = store
            .push_line("api", r#"{"level":"error","msg":"d"}"#)
            .await;
        let id_info = e1[0].id - 1; // first info is before first error... use e1 id
        let from = e1[0].id;
        let next = store
            .find_level_near(
                from,
                NavDirection::Next,
                &["error"],
                Some("api"),
                &CompiledQuery::MatchAll,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(next.id, e2[0].id);

        let prev = store
            .find_level_near(
                e2[0].id,
                NavDirection::Prev,
                &["error"],
                None,
                &CompiledQuery::MatchAll,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(prev.id, e1[0].id);
        let _ = id_info;
    }

    #[tokio::test]
    async fn empty_levels_match_all_and_filters_skip() {
        let store = Store::new(1_000_000);
        store.push_line("api", r#"{"level":"info","msg":"a"}"#).await;
        let mid = store
            .push_line("web", r#"{"level":"warn","msg":"b"}"#)
            .await;
        store
            .push_line("api", r#"{"level":"error","msg":"c"}"#)
            .await;

        let from = mid[0].id;
        let any = store
            .find_level_near(
                from - 1,
                NavDirection::Next,
                &[],
                None,
                &CompiledQuery::MatchAll,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(any.id, from);

        let svc_skip = store
            .find_level_near(
                from - 1,
                NavDirection::Next,
                &["warn"],
                Some("api"),
                &CompiledQuery::MatchAll,
                None,
                None,
            )
            .await;
        assert!(svc_skip.is_none());

        let cel_skip = store
            .find_level_near(
                from - 1,
                NavDirection::Next,
                &["warn"],
                None,
                &compile_query(r#"msg == "missing""#).unwrap(),
                None,
                None,
            )
            .await;
        assert!(cel_skip.is_none());

        let time_skip = store
            .find_level_near(
                from,
                NavDirection::Next,
                &["error"],
                None,
                &CompiledQuery::MatchAll,
                Some(chrono::Utc::now() + chrono::Duration::hours(1)),
                None,
            )
            .await;
        assert!(time_skip.is_none());
    }

    #[tokio::test]
    async fn find_level_near_reads_severity_and_lvl_aliases() {
        let store = Store::new(1_000_000);
        let first = store
            .push_line("api", r#"{"severity":"info","msg":"a"}"#)
            .await;
        let mid = store
            .push_line("api", r#"{"lvl":"error","msg":"b"}"#)
            .await;
        store
            .push_line("api", r#"{"level":"info","msg":"c"}"#)
            .await;

        let err = store
            .find_level_near(
                first[0].id,
                NavDirection::Next,
                &["error"],
                None,
                &CompiledQuery::MatchAll,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(err.id, mid[0].id);
        assert_eq!(err.data["msg"], "b");
    }
}
