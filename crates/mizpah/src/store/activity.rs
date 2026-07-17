//! Activity histogram over the in-memory buffer.

use super::{ActivityBucket, Store};
use chrono::{DateTime, Duration as ChronoDuration, Utc};

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

        let mut counts = vec![0u64; n_buckets];
        {
            let inner = self.inner.read().await;
            for entry in &inner.entries {
                let ts_ms = entry.received_at.timestamp_millis();
                if ts_ms < window_start_ms {
                    continue;
                }
                let offset = ts_ms - window_start_ms;
                let idx = (offset / bucket_ms) as usize;
                if idx < n_buckets {
                    counts[idx] += 1;
                }
            }
        }

        counts
            .into_iter()
            .enumerate()
            .map(|(i, count)| {
                let start = window_start + ChronoDuration::milliseconds(bucket_ms * i as i64);
                let end = start + ChronoDuration::milliseconds(bucket_ms);
                ActivityBucket { start, end, count }
            })
            .collect()
    }
}
