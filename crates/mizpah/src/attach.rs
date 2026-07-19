use crate::mzp_meta::MzpMeta;
use crate::stdin_lines::for_each_line;
use serde::Serialize;
use tokio::io::{AsyncBufRead, BufReader};
use tracing::{error, info, warn};

#[derive(Serialize)]
struct IngestBody<'a> {
    service: &'a str,
    line: &'a str,
    mzp: &'a MzpMeta,
}

type AttachError = Box<dyn std::error::Error + Send + Sync>;

async fn health_check(client: &reqwest::Client, base_url: &str) -> Result<(), AttachError> {
    let health = format!("{base_url}/api/stats");
    let resp = client
        .get(&health)
        .send()
        .await
        .map_err(|e| format!("could not reach Mizpah hub at {base_url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Mizpah hub at {base_url} returned {}", resp.status()).into());
    }
    Ok(())
}

async fn forward_line(
    client: &reqwest::Client,
    ingest_url: &str,
    service: &str,
    line: &str,
    mzp: &MzpMeta,
) -> Result<(), AttachError> {
    let body = IngestBody {
        service,
        line,
        mzp,
    };
    match client.post(ingest_url).json(&body).send().await {
        Ok(r) if r.status().is_success() => Ok(()),
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

/// Forward lines from `reader` to an existing hub.
pub async fn attach_from_reader<R>(
    client: reqwest::Client,
    base_url: &str,
    service: &str,
    reader: R,
) -> Result<(), AttachError>
where
    R: AsyncBufRead + Unpin,
{
    health_check(&client, base_url).await?;
    info!(%service, %base_url, "attached to hub; forwarding stdin");

    let ingest_url = format!("{base_url}/api/ingest");
    let service = service.to_string();
    let mzp = MzpMeta::capture();

    for_each_line(reader, |line| {
        let client = client.clone();
        let ingest_url = ingest_url.clone();
        let service = service.clone();
        let mzp = mzp.clone();
        async move { forward_line(&client, &ingest_url, &service, &line, &mzp).await }
    })
    .await?;

    info!(%service, "stdin closed; attach process exiting");
    Ok(())
}

/// Forward stdin lines to an existing Mizpah hub via POST /api/ingest.
pub async fn attach_and_forward(
    base_url: &str,
    service: &str,
) -> Result<(), AttachError> {
    crate::util::ensure_rustls_crypto_provider();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    attach_from_reader(
        client,
        base_url,
        service,
        BufReader::new(tokio::io::stdin()),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::spawn_test_hub;
    use std::io::Cursor;

    #[cfg(not(miri))]
    #[tokio::test]
    async fn attach_from_reader_forwards_lines() {
        crate::util::ensure_rustls_crypto_provider();
        let (url, store) = spawn_test_hub().await;
        let client = reqwest::Client::new();
        let data = b"{\"msg\":\"from-attach\"}\n";
        attach_from_reader(
            client,
            &url,
            "svc",
            BufReader::new(Cursor::new(&data[..])),
        )
        .await
        .unwrap();
        let (entries, _) = store
            .query_logs(
                Some("svc"),
                None,
                10,
                &crate::filter::CompiledQuery::MatchAll,
                None,
                None,
            )
            .await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].data["msg"], serde_json::json!("from-attach"));
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn attach_health_fails_on_bad_url() {
        crate::util::ensure_rustls_crypto_provider();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(200))
            .build()
            .unwrap();
        let err = attach_from_reader(
            client,
            "http://127.0.0.1:1",
            "svc",
            BufReader::new(Cursor::new(&b"x\n"[..])),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("could not reach"));
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn attach_exits_on_disconnect_conflict() {
        crate::util::ensure_rustls_crypto_provider();
        let (url, store) = spawn_test_hub().await;
        store.disconnect_service("svc").await;
        let client = reqwest::Client::new();
        let err = attach_from_reader(
            client,
            &url,
            "svc",
            BufReader::new(Cursor::new(&b"{\"a\":1}\n"[..])),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("disconnected"));
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn health_check_rejects_non_success() {
        use axum::{routing::get, Router};
        use std::net::SocketAddr;
        use tokio::net::TcpListener;

        crate::util::ensure_rustls_crypto_provider();
        let app =
            Router::new().route("/api/stats", get(|| async { axum::http::StatusCode::INTERNAL_SERVER_ERROR }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::new();
        let err = health_check(&client, &format!("http://{addr}"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("returned"));
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn attach_warns_on_hub_reject_but_continues() {
        use axum::{routing::post, Router};
        use std::net::SocketAddr;
        use tokio::net::TcpListener;

        crate::util::ensure_rustls_crypto_provider();
        let app = Router::new().route(
            "/api/stats",
            axum::routing::get(|| async { axum::http::StatusCode::OK }),
        ).route(
            "/api/ingest",
            post(|| async { axum::http::StatusCode::BAD_REQUEST }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base = format!("http://{addr}");
        let client = reqwest::Client::new();
        attach_from_reader(
            client,
            &base,
            "svc",
            BufReader::new(Cursor::new(&b"{\"msg\":\"x\"}\n"[..])),
        )
        .await
        .unwrap();
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn attach_and_forward_eof_on_empty_stdin() {
        let (url, _) = spawn_test_hub().await;
        // Test process stdin is typically closed/empty → health check then EOF.
        attach_and_forward(&url, "svc").await.unwrap();
    }
}
