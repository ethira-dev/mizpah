//! Activity histogram over the in-memory buffer.

use super::{ActivityBucket, Store};
use chrono::{DateTime, Duration as ChronoDuration, Utc};

#[derive(Clone, Copy, Default)]
struct LevelCounts {
    total: u64,
    error: u64,
    warn: u64,
    other: u64,
}

fn classify_level(data: &serde_json::Value) -> &'static str {
    let level = data
        .get("level")
        .or_else(|| data.get("severity"))
        .or_else(|| data.get("lvl"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if level == "error" || level == "err" || level == "fatal" || level == "critical" {
        "error"
    } else if level == "warn" || level == "warning" {
        "warn"
    } else {
        "other"
    }
}

impl Store {
    pub async fn activity_histogram(
        &self,
        window: ChronoDuration,
        bucket: ChronoDuration,
    ) -> Vec<ActivityBucket> {
        let window = if window <= ChronoDuration::zero() {
            ChronoDuration::hours(24)
        } else {
            window
        };
        let bucket = if bucket <= ChronoDuration::zero() {
            ChronoDuration::minutes(25)
        } else {
            bucket
        };

        let now = Utc::now();
        let bucket_ms = bucket.num_milliseconds().max(1);
        let window_ms = window.num_milliseconds().max(1);
        let n_buckets = ((window_ms + bucket_ms - 1) / bucket_ms) as usize;
        let n_buckets = n_buckets.max(1);

        // Align to a fixed UTC grid so bucket boundaries stay stable across polls.
        let now_ms = now.timestamp_millis();
        let current_bucket_end = ((now_ms / bucket_ms) + 1) * bucket_ms;
        let window_start_ms = current_bucket_end - bucket_ms * n_buckets as i64;
        let window_start =
            DateTime::from_timestamp_millis(window_start_ms).unwrap_or_else(|| now - window);

        let mut counts = vec![LevelCounts::default(); n_buckets];
        {
            let inner = self.inner.read().await;
            for entry in &inner.entries {
                let ts_ms = entry.effective_event_time().timestamp_millis();
                if ts_ms < window_start_ms {
                    continue;
                }
                let offset = ts_ms - window_start_ms;
                let idx = (offset / bucket_ms) as usize;
                if idx < n_buckets {
                    counts[idx].total += 1;
                    match classify_level(&entry.data) {
                        "error" => counts[idx].error += 1,
                        "warn" => counts[idx].warn += 1,
                        _ => counts[idx].other += 1,
                    }
                }
            }
        }

        counts
            .into_iter()
            .enumerate()
            .map(|(i, c)| {
                let start = window_start + ChronoDuration::milliseconds(bucket_ms * i as i64);
                let end = start + ChronoDuration::milliseconds(bucket_ms);
                ActivityBucket {
                    start,
                    end,
                    count: c.total,
                    error: c.error,
                    warn: c.warn,
                    other: c.other,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use chrono::Utc;

    #[tokio::test]
    async fn activity_level_split_and_event_time_window() {
        let store = Store::new(1_000_000);
        // Old event outside a 24h window
        store
            .push_line(
                "api",
                r#"{"level":"error","msg":"old","@timestamp":"2020-01-01T00:00:00Z"}"#,
            )
            .await;
        store
            .push_line("api", r#"{"level":"error","msg":"now"}"#)
            .await;
        store
            .push_line("api", r#"{"level":"warn","msg":"now"}"#)
            .await;
        store
            .push_line("api", r#"{"level":"info","msg":"now"}"#)
            .await;
        store
            .push_line("api", r#"{"severity":"error","msg":"sev"}"#)
            .await;
        store
            .push_line("api", r#"{"lvl":"warn","msg":"lvl"}"#)
            .await;

        let buckets = store
            .activity_histogram(ChronoDuration::hours(24), ChronoDuration::hours(1))
            .await;
        let total_error: u64 = buckets.iter().map(|b| b.error).sum();
        let total_warn: u64 = buckets.iter().map(|b| b.warn).sum();
        let total_other: u64 = buckets.iter().map(|b| b.other).sum();
        // 2020 event excluded by event_time window; severity/lvl aliases count.
        assert_eq!(total_error, 2); // level=error + severity=error
        assert_eq!(total_warn, 2); // level=warn + lvl=warn
        assert_eq!(total_other, 1); // info
        let _ = Utc;
    }

    #[tokio::test]
    async fn zero_window_and_bucket_use_defaults() {
        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"level":"info","msg":"now"}"#)
            .await;
        let buckets = store
            .activity_histogram(ChronoDuration::zero(), ChronoDuration::zero())
            .await;
        assert_eq!(buckets.len(), 58); // default 24h / 25m
        assert_eq!(buckets.iter().map(|b| b.count).sum::<u64>(), 1);
    }

    #[tokio::test]
    async fn future_event_outside_last_bucket_is_skipped() {
        let store = Store::new(1_000_000);
        store
            .push_line(
                "api",
                r#"{"level":"error","msg":"future","@timestamp":"2099-06-01T12:00:00Z"}"#,
            )
            .await;
        store
            .push_line("api", r#"{"level":"info","msg":"now"}"#)
            .await;
        let buckets = store
            .activity_histogram(ChronoDuration::hours(1), ChronoDuration::hours(1))
            .await;
        let total: u64 = buckets.iter().map(|b| b.count).sum();
        assert_eq!(total, 1);
    }

    #[tokio::test]
    async fn activity_classifies_severity_and_lvl_aliases() {
        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"severity":"error","msg":"sev"}"#)
            .await;
        store
            .push_line("api", r#"{"lvl":"warn","msg":"lvl"}"#)
            .await;
        let buckets = store
            .activity_histogram(ChronoDuration::hours(24), ChronoDuration::hours(1))
            .await;
        let total_error: u64 = buckets.iter().map(|b| b.error).sum();
        let total_warn: u64 = buckets.iter().map(|b| b.warn).sum();
        assert_eq!(total_error, 1);
        assert_eq!(total_warn, 1);
    }

    #[tokio::test]
    async fn activity_err_and_fatal_levels_count_as_error() {
        let store = Store::new(1_000_000);
        store.push_line("api", r#"{"level":"err","msg":"e"}"#).await;
        store
            .push_line("api", r#"{"level":"fatal","msg":"f"}"#)
            .await;
        let buckets = store
            .activity_histogram(ChronoDuration::hours(24), ChronoDuration::hours(1))
            .await;
        let total_error: u64 = buckets.iter().map(|b| b.error).sum();
        assert_eq!(total_error, 2);
    }
}
