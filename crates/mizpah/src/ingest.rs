use crate::mzp_meta::MzpMeta;
use crate::stdin_lines::for_each_stdin_line;
use crate::store::Store;
use std::convert::Infallible;
use std::sync::Arc;
use tracing::{debug, info};

/// Read stdin line-by-line and push into the hub store.
///
/// When the service is disconnected (blocked), lines are drained and discarded
/// so the pipe does not back up while the hub stays available.
pub async fn ingest_stdin_local(store: Arc<Store>, service: String) {
    info!(%service, "ingesting stdin into local store");
    let mzp = MzpMeta::capture();

    let _ = for_each_stdin_line(|line| {
        let store = Arc::clone(&store);
        let service = service.clone();
        let mzp = mzp.clone();
        async move {
            if store.is_blocked(&service).await {
                debug!(%service, "discarding stdin line; service disconnected");
                return Ok::<(), Infallible>(());
            }
            store
                .push_line_with_meta(&service, &line, None, Some(&mzp))
                .await;
            Ok::<(), Infallible>(())
        }
    })
    .await;

    info!(%service, "stdin closed");
    // Keep the hub alive after stdin closes so the UI remains available.
    debug!(%service, "stdin ingest task finished; server continues running");
}
