use crate::mzp_meta::MzpMeta;
use crate::stdin_lines::for_each_line;
use crate::store::Store;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::io::{AsyncBufRead, BufReader};
use tracing::{debug, info};

/// Read stdin line-by-line and push into the hub store.
///
/// When the service is disconnected (blocked), lines are drained and discarded
/// so the pipe does not back up while the hub stays available.
pub async fn ingest_stdin_local(store: Arc<Store>, service: String) {
    info!(%service, "ingesting stdin into local store");
    ingest_from_reader(store, service, BufReader::new(tokio::io::stdin())).await;
}

async fn push_ingest_line(store: &Store, service: &str, line: &str, mzp: &MzpMeta) {
    match store
        .push_line_with_meta(service, line, None, Some(mzp))
        .await
    {
        crate::store::PushLineResult::Blocked => {
            debug!(%service, "discarding stdin line; service disconnected");
        }
        crate::store::PushLineResult::Emitted(_) => {}
    }
}

/// Ingest lines from an arbitrary buffered reader into the hub store.
pub async fn ingest_from_reader<R>(store: Arc<Store>, service: String, reader: R)
where
    R: AsyncBufRead + Unpin,
{
    let mzp = MzpMeta::capture();
    let _ = for_each_line(reader, |line| {
        let store = Arc::clone(&store);
        let service = service.clone();
        let mzp = mzp.clone();
        async move {
            push_ingest_line(&store, &service, &line, &mzp).await;
            Ok::<(), Infallible>(())
        }
    })
    .await;

    info!(%service, "stdin closed");
    // Keep the hub alive after stdin closes so the UI remains available.
    debug!(%service, "stdin ingest task finished; server continues running");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::CompiledQuery;
    use std::io::Cursor;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn ingest_from_reader_emits_lines() {
        let store = Arc::new(Store::new(1_000_000));
        let data = b"{\"msg\":\"hi\"}\n\nplain\n";
        ingest_from_reader(
            Arc::clone(&store),
            "api".into(),
            BufReader::new(Cursor::new(&data[..])),
        )
        .await;
        let (entries, _) = store
            .query_logs(Some("api"), None, 10, &CompiledQuery::MatchAll, None, None)
            .await;
        assert_eq!(entries.len(), 2);
    }

    #[tokio::test]
    async fn ingest_from_reader_discards_when_blocked() {
        let store = Arc::new(Store::new(1_000_000));
        store.disconnect_service("api").await;
        let data = b"{\"msg\":\"x\"}\n";
        ingest_from_reader(
            Arc::clone(&store),
            "api".into(),
            BufReader::new(Cursor::new(&data[..])),
        )
        .await;
        let (entries, _) = store
            .query_logs(Some("api"), None, 10, &CompiledQuery::MatchAll, None, None)
            .await;
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn ingest_stdin_local_eof() {
        let store = Arc::new(Store::new(1_000_000));
        ingest_stdin_local(store, "api".into()).await;
    }
}
