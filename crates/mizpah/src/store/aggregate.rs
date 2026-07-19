//! Group-by aggregation over the in-memory log buffer.

use super::ingest::in_time_range;
use super::Store;
use crate::filter::CompiledQuery;
use crate::properties::get_at_path;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

const MAX_AGGREGATE_ROWS: usize = 100;

/// Which numeric aggregates to compute on an optional field.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregateMetrics {
    /// Dot-path to a numeric field (e.g. `duration_ms`). Required for sum/avg/min/max.
    #[serde(default)]
    pub field: Option<String>,
    #[serde(default)]
    pub sum: bool,
    #[serde(default)]
    pub avg: bool,
    #[serde(default)]
    pub min: bool,
    #[serde(default)]
    pub max: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregateRow {
    /// Stringified values for each `group_by` path (same order).
    pub keys: Vec<String>,
    pub count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sum: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

#[derive(Default)]
struct Acc {
    count: u64,
    sum: f64,
    min: Option<f64>,
    max: Option<f64>,
    numeric_n: u64,
}

fn value_key(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(other) => other.to_string(),
    }
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

impl Store {
    #[allow(clippy::too_many_arguments)]
    pub async fn aggregate_logs(
        &self,
        service: Option<&str>,
        query: &CompiledQuery,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        group_by: &[String],
        metrics: &AggregateMetrics,
        limit: usize,
    ) -> Vec<AggregateRow> {
        let limit = limit.clamp(1, MAX_AGGREGATE_ROWS);
        let metric_field = metrics
            .field
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let want_numeric =
            metric_field.is_some() && (metrics.sum || metrics.avg || metrics.min || metrics.max);

        let mut map: HashMap<Vec<String>, Acc> = HashMap::new();
        {
            let inner = self.inner.read().await;
            for entry in &inner.entries {
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

                let keys: Vec<String> = if group_by.is_empty() {
                    vec!["*".into()]
                } else {
                    group_by
                        .iter()
                        .map(|path| {
                            if path == "service" {
                                entry.service.clone()
                            } else {
                                value_key(get_at_path(&entry.data, path))
                            }
                        })
                        .collect()
                };

                let acc = map.entry(keys).or_default();
                acc.count += 1;
                if want_numeric {
                    if let Some(path) = metric_field {
                        if let Some(n) = get_at_path(&entry.data, path).and_then(as_f64) {
                            acc.sum += n;
                            acc.numeric_n += 1;
                            acc.min = Some(acc.min.map_or(n, |m| m.min(n)));
                            acc.max = Some(acc.max.map_or(n, |m| m.max(n)));
                        }
                    }
                }
            }
        }

        let mut rows: Vec<AggregateRow> = map
            .into_iter()
            .map(|(keys, acc)| {
                let avg = if metrics.avg && acc.numeric_n > 0 {
                    Some(acc.sum / acc.numeric_n as f64)
                } else {
                    None
                };
                AggregateRow {
                    keys,
                    count: acc.count,
                    sum: if metrics.sum && acc.numeric_n > 0 {
                        Some(acc.sum)
                    } else {
                        None
                    },
                    avg,
                    min: if metrics.min { acc.min } else { None },
                    max: if metrics.max { acc.max } else { None },
                }
            })
            .collect();

        rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.keys.cmp(&b.keys)));
        rows.truncate(limit);
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{compile_query, CompiledQuery};
    use crate::store::Store;

    #[tokio::test]
    async fn aggregates_by_level() {
        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"level":"error","duration_ms":10}"#)
            .await;
        store
            .push_line("api", r#"{"level":"error","duration_ms":20}"#)
            .await;
        store
            .push_line("api", r#"{"level":"info","duration_ms":5}"#)
            .await;

        let metrics = AggregateMetrics {
            field: Some("duration_ms".into()),
            sum: true,
            avg: true,
            min: true,
            max: true,
        };
        let rows = store
            .aggregate_logs(
                Some("api"),
                &CompiledQuery::MatchAll,
                None,
                None,
                &["level".into()],
                &metrics,
                10,
            )
            .await;
        assert_eq!(rows.len(), 2);
        let err = rows.iter().find(|r| r.keys == ["error"]).unwrap();
        assert_eq!(err.count, 2);
        assert_eq!(err.sum, Some(30.0));
        assert_eq!(err.avg, Some(15.0));
        assert_eq!(err.min, Some(10.0));
        assert_eq!(err.max, Some(20.0));
    }

    #[tokio::test]
    async fn value_key_types_and_empty_group_by() {
        let store = Store::new(1_000_000);
        store
            .push_line(
                "api",
                r#"{"flag":true,"count":42,"meta":{"k":"v"},"level":"info"}"#,
            )
            .await;
        store
            .push_line("web", r#"{"flag":false,"count":1,"level":"info"}"#)
            .await;

        let metrics = AggregateMetrics::default();
        let rows = store
            .aggregate_logs(
                None,
                &CompiledQuery::MatchAll,
                None,
                None,
                &[],
                &metrics,
                10,
            )
            .await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].keys, vec!["*".to_string()]);
        assert_eq!(rows[0].count, 2);

        let typed = store
            .aggregate_logs(
                Some("api"),
                &CompiledQuery::MatchAll,
                None,
                None,
                &["flag".into(), "count".into(), "meta".into()],
                &metrics,
                10,
            )
            .await;
        assert_eq!(typed.len(), 1);
        assert_eq!(typed[0].keys[0], "true");
        assert_eq!(typed[0].keys[1], "42");
        assert!(typed[0].keys[2].contains("k"));
    }

    #[tokio::test]
    async fn service_time_and_cel_filters() {
        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"level":"error","msg":"a"}"#)
            .await;
        store
            .push_line("web", r#"{"level":"error","msg":"b"}"#)
            .await;
        store
            .push_line(
                "api",
                r#"{"level":"info","msg":"old","@timestamp":"2020-01-01T00:00:00Z"}"#,
            )
            .await;

        let metrics = AggregateMetrics::default();
        let api_only = store
            .aggregate_logs(
                Some("api"),
                &CompiledQuery::MatchAll,
                None,
                None,
                &["level".into()],
                &metrics,
                10,
            )
            .await;
        assert_eq!(api_only.iter().map(|r| r.count).sum::<u64>(), 2);

        let wildcard = store
            .aggregate_logs(
                Some("*"),
                &CompiledQuery::MatchAll,
                None,
                None,
                &["service".into()],
                &metrics,
                10,
            )
            .await;
        assert_eq!(wildcard.len(), 2);

        let cel = store
            .aggregate_logs(
                None,
                &compile_query(r#"msg == "a""#).unwrap(),
                Some(chrono::Utc::now() - chrono::Duration::hours(1)),
                None,
                &["level".into()],
                &metrics,
                10,
            )
            .await;
        assert_eq!(cel.len(), 1);
        assert_eq!(cel[0].keys, vec!["error".to_string()]);
    }

    #[tokio::test]
    async fn numeric_metric_parses_string_values() {
        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"level":"info","duration_ms":"15"}"#)
            .await;
        let metrics = AggregateMetrics {
            field: Some("duration_ms".into()),
            sum: true,
            avg: false,
            min: true,
            max: true,
        };
        let rows = store
            .aggregate_logs(
                Some(""),
                &CompiledQuery::MatchAll,
                None,
                None,
                &["level".into()],
                &metrics,
                10,
            )
            .await;
        assert_eq!(rows[0].sum, Some(15.0));
    }

    #[tokio::test]
    async fn null_group_key_and_non_numeric_metric_skipped() {
        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"level":null,"duration_ms":"nope"}"#)
            .await;
        let metrics = AggregateMetrics {
            field: Some("duration_ms".into()),
            sum: true,
            avg: true,
            min: true,
            max: true,
        };
        let rows = store
            .aggregate_logs(
                Some("api"),
                &CompiledQuery::MatchAll,
                None,
                None,
                &["level".into()],
                &metrics,
                10,
            )
            .await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].keys, vec!["".to_string()]);
        assert_eq!(rows[0].count, 1);
        assert!(rows[0].sum.is_none());
        assert!(rows[0].avg.is_none());
    }
}
