use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::{error, info, warn};

#[derive(Serialize)]
struct IngestBody<'a> {
    service: &'a str,
    line: &'a str,
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
    let resp = client.get(&health).send().await.map_err(|e| {
        format!("could not reach Mizpah hub at {base_url}: {e}")
    })?;
    if !resp.status().is_success() {
        return Err(format!(
            "Mizpah hub at {base_url} returned {}",
            resp.status()
        )
        .into());
    }

    info!(%service, %base_url, "attached to hub; forwarding stdin");

    let ingest_url = format!("{base_url}/api/ingest");
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();

    loop {
        match reader.next_line().await {
            Ok(Some(line)) => {
                if line.is_empty() {
                    continue;
                }
                let body = IngestBody {
                    service,
                    line: &line,
                };
                match client.post(&ingest_url).json(&body).send().await {
                    Ok(r) if r.status().is_success() => {}
                    Ok(r) => {
                        warn!(status = %r.status(), "hub rejected ingest");
                    }
                    Err(err) => {
                        error!(error = %err, "failed to forward line to hub");
                        return Err(err.into());
                    }
                }
            }
            Ok(None) => {
                info!(%service, "stdin closed; attach process exiting");
                break;
            }
            Err(err) => {
                warn!(error = %err, "failed reading stdin");
                break;
            }
        }
    }

    Ok(())
}
