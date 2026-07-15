use crate::stdin_lines::for_each_stdin_line;
use crate::store::Store;
use std::convert::Infallible;
use std::sync::Arc;
use tracing::{debug, info};

/// Read stdin line-by-line and push into the hub store.
pub async fn ingest_stdin_local(store: Arc<Store>, service: String) {
    info!(%service, "ingesting stdin into local store");

    let _ = for_each_stdin_line(|line| {
        let store = Arc::clone(&store);
        let service = service.clone();
        async move {
            store.push_line(&service, &line).await;
            Ok::<(), Infallible>(())
        }
    })
    .await;

    info!(%service, "stdin closed");
    // Keep the hub alive after stdin closes so the UI remains available.
    debug!(%service, "stdin ingest task finished; server continues running");
}
