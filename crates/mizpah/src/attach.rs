use crate::mzp_meta::MzpMeta;
use crate::stdin_lines::for_each_stdin_line;
use serde::Serialize;
use tracing::{error, info, warn};

#[derive(Serialize)]
struct IngestBody<'a> {
    service: &'a str,
    line: &'a str,
    mzp: &'a MzpMeta,
}

/// Forward stdin lines to an existing Mizpah hub via POST /api/ingest.
pub async fn attach_and_forward(
    base_url: &str,
    service: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let health = format!("{base_url}/api/stats");
    let resp = client
        .get(&health)
        .send()
        .await
        .map_err(|e| format!("could not reach Mizpah hub at {base_url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Mizpah hub at {base_url} returned {}", resp.status()).into());
    }

    info!(%service, %base_url, "attached to hub; forwarding stdin");

    let ingest_url = format!("{base_url}/api/ingest");
    let service = service.to_string();
    let mzp = MzpMeta::capture();

    type AttachError = Box<dyn std::error::Error + Send + Sync>;

    for_each_stdin_line(|line| {
        let client = client.clone();
        let ingest_url = ingest_url.clone();
        let service = service.clone();
        let mzp = mzp.clone();
        async move {
            let body = IngestBody {
                service: &service,
                line: &line,
                mzp: &mzp,
            };
            match client.post(&ingest_url).json(&body).send().await {
                Ok(r) if r.status().is_success() => Ok::<(), AttachError>(()),
                Ok(r) if r.status() == reqwest::StatusCode::CONFLICT => {
                    warn!(%service, "service disconnected on hub; attach exiting");
                    Err("service disconnected".into())
                }
                Ok(r) => {
                    warn!(status = %r.status(), "hub rejected ingest");
                    Ok(())
                }
                Err(err) => {
                    error!(error = %err, "failed to forward line to hub");
                    Err(err.into())
                }
            }
        }
    })
    .await?;

    info!(%service, "stdin closed; attach process exiting");
    Ok(())
}
