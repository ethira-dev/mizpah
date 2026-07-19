//! Time × value heat-map over a field path.

use super::Store;
use crate::properties::get_at_path;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpectrogramResult {
    pub field_path: String,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    /// Start time of each time bucket (length = time_buckets).
    pub time_starts: Vec<DateTime<Utc>>,
    /// Top value labels (length ≤ value_buckets), plus optional `"__other__"`.
    pub value_labels: Vec<String>,
    /// `counts[time_idx][value_idx]`.
    pub counts: Vec<Vec<u64>>,
}

fn value_label(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "(null)".into(),
        Some(Value::String(s)) => {
            if s.is_empty() {
                "(empty)".into()
            } else {
                s.clone()
            }
        }
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(other) => other.to_string(),
    }
}

impl Store {
    pub async fn spectrogram(
        &self,
        field_path: &str,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        time_buckets: usize,
        value_buckets: usize,
    ) -> SpectrogramResult {
        let time_buckets = time_buckets.clamp(1, 120);
        let value_buckets = value_buckets.clamp(1, 40);
        let to = to.unwrap_or_else(Utc::now);
        let from = from.unwrap_or_else(|| to - ChronoDuration::hours(1));
        let span_ms = (to - from).num_milliseconds().max(1);
        let bucket_ms = (span_ms + time_buckets as i64 - 1) / time_buckets as i64;
        let bucket_ms = bucket_ms.max(1);

        let mut value_totals: HashMap<String, u64> = HashMap::new();
        let mut raw: Vec<(usize, String)> = Vec::new();
        {
            let inner = self.inner.read().await;
            for entry in &inner.entries {
                let t = entry.effective_event_time();
                if t < from || t >= to {
                    continue;
                }
                let offset = (t - from).num_milliseconds().max(0);
                let idx = ((offset / bucket_ms) as usize).min(time_buckets - 1);
                let label = if field_path == "service" {
                    entry.service.clone()
                } else {
                    value_label(get_at_path(&entry.data, field_path))
                };
                *value_totals.entry(label.clone()).or_insert(0) += 1;
                raw.push((idx, label));
            }
        }

        let mut ranked: Vec<(String, u64)> = value_totals.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let top_n = value_buckets.saturating_sub(1).max(1);
        let mut value_labels: Vec<String> =
            ranked.iter().take(top_n).map(|(k, _)| k.clone()).collect();
        let has_other = ranked.len() > value_labels.len();
        if has_other {
            value_labels.push("__other__".into());
        }
        let label_index: HashMap<&str, usize> = value_labels
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), i))
            .collect();
        let other_idx = label_index.get("__other__").copied();

        let mut counts = vec![vec![0u64; value_labels.len()]; time_buckets];
        for (t_idx, label) in raw {
            let v_idx = label_index
                .get(label.as_str())
                .copied()
                .or(other_idx)
                .unwrap_or(0);
            if let Some(row) = counts.get_mut(t_idx) {
                if let Some(cell) = row.get_mut(v_idx) {
                    *cell += 1;
                }
            }
        }

        let time_starts: Vec<DateTime<Utc>> = (0..time_buckets)
            .map(|i| from + ChronoDuration::milliseconds(bucket_ms * i as i64))
            .collect();

        SpectrogramResult {
            field_path: field_path.to_string(),
            from,
            to,
            time_starts,
            value_labels,
            counts,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::store::Store;

    #[tokio::test]
    async fn spectrogram_buckets_levels() {
        let store = Store::new(1_000_000);
        store.push_line("api", r#"{"level":"error"}"#).await;
        store.push_line("api", r#"{"level":"info"}"#).await;
        store.push_line("api", r#"{"level":"error"}"#).await;
        let result = store.spectrogram("level", None, None, 4, 8).await;
        assert_eq!(result.field_path, "level");
        assert!(!result.counts.is_empty());
        let total: u64 = result.counts.iter().flatten().sum();
        assert_eq!(total, 3);
    }

    #[tokio::test]
    async fn value_label_variants_service_field_and_window() {
        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"level":"","flag":true,"n":7,"arr":[1]}"#)
            .await;
        store.push_line("web", r#"{"level":"info"}"#).await;
        store
            .push_line(
                "api",
                r#"{"level":"old","@timestamp":"2020-01-01T00:00:00Z"}"#,
            )
            .await;
        for i in 0..6 {
            store
                .push_line("api", &format!(r#"{{"level":"v{i}"}}"#))
                .await;
        }

        let by_level = store.spectrogram("level", None, None, 2, 3).await;
        assert!(by_level.value_labels.contains(&"(empty)".into()));
        assert!(by_level.value_labels.contains(&"__other__".into()));

        let by_service = store.spectrogram("service", None, None, 2, 4).await;
        assert!(by_service.value_labels.iter().any(|l| l == "api"));
        assert!(by_service.value_labels.iter().any(|l| l == "web"));

        let null_field = store.spectrogram("missing", None, None, 2, 4).await;
        assert!(null_field.value_labels.contains(&"(null)".into()));

        let bool_field = store.spectrogram("flag", None, None, 2, 4).await;
        assert!(bool_field.value_labels.iter().any(|l| l == "true"));

        let num_field = store.spectrogram("n", None, None, 2, 4).await;
        assert!(num_field.value_labels.iter().any(|l| l == "7"));

        let arr_field = store.spectrogram("arr", None, None, 2, 4).await;
        assert!(arr_field.value_labels.iter().any(|l| l.contains('[')));

        let total: u64 = by_level.counts.iter().flatten().sum();
        assert!(total < 10); // old entry excluded from default 1h window
    }

    #[tokio::test]
    async fn spectrogram_increments_existing_bucket_cell() {
        let store = Store::new(1_000_000);
        store.push_line("api", r#"{"level":"info"}"#).await;
        store.push_line("api", r#"{"level":"info"}"#).await;
        let result = store.spectrogram("level", None, None, 4, 8).await;
        let total: u64 = result.counts.iter().flatten().sum();
        assert_eq!(total, 2);
        let info_idx = result
            .value_labels
            .iter()
            .position(|l| l == "info")
            .unwrap();
        let cell_max = result
            .counts
            .iter()
            .map(|row| row.get(info_idx).copied().unwrap_or(0))
            .max()
            .unwrap_or(0);
        assert!(cell_max >= 2);
    }
}
