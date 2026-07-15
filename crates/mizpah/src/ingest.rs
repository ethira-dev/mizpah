use crate::store::Store;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::{debug, info, warn};

/// Read stdin line-by-line and push into the hub store.
pub async fn ingest_stdin_local(store: Arc<Store>, service: String) {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    info!(%service, "ingesting stdin into local store");

    loop {
        match reader.next_line().await {
            Ok(Some(line)) => {
                if line.is_empty() {
                    continue;
                }
                store.push_line(&service, &line).await;
            }
            Ok(None) => {
                info!(%service, "stdin closed");
                break;
            }
            Err(err) => {
                warn!(%service, error = %err, "failed reading stdin");
                break;
            }
        }
    }

    // Keep the hub alive after stdin closes so the UI remains available.
    debug!(%service, "stdin ingest task finished; server continues running");
}
