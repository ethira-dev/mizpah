//! Axum HTTP / WebSocket API for the hub.

mod routes;
mod static_files;
mod ws;

use axum::routing::{get, post};
use axum::Router;
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::CorsLayer;

use crate::store::Store;
use crate::update::UpdateManager;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub project_dir: PathBuf,
    pub update: Arc<UpdateManager>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/ingest", post(routes::ingest))
        .route("/api/ingest/batch", post(routes::ingest_batch))
        .route("/api/logs", get(routes::list_logs))
        .route("/api/services", get(routes::list_services))
        .route("/api/services/disconnect", post(routes::disconnect_service))
        .route("/api/services/reconnect", post(routes::reconnect_service))
        .route("/api/properties", get(routes::list_properties))
        .route("/api/stats", get(routes::stats))
        .route("/api/activity", get(routes::activity))
        .route("/api/investigate", post(routes::investigate))
        .route(
            "/api/update",
            get(routes::get_update).post(routes::post_update),
        )
        .route("/ws", get(ws::ws_handler))
        .fallback(static_files::static_handler)
        .layer(CorsLayer::permissive())
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::CompiledQuery;
    use crate::update;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

    fn test_state(store: Arc<Store>) -> AppState {
        AppState {
            store,
            project_dir: std::env::temp_dir(),
            update: update::UpdateManager::new(update::RestartContext {
                host: "127.0.0.1".into(),
                port: 1738,
                project_dir: std::env::temp_dir(),
                max_bytes: 1_000_000,
            }),
        }
    }

    fn test_app() -> Router {
        router(test_state(Arc::new(Store::new(1_000_000))))
    }

    #[tokio::test]
    async fn ingest_single_line() {
        let store = Arc::new(Store::new(1_000_000));
        let app = router(test_state(Arc::clone(&store)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ingest")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"service":"api","line":"{\"msg\":\"hi\"}"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let (entries, _) = store
            .query_logs(Some("api"), None, 1, &CompiledQuery::MatchAll, None, None)
            .await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].data["msg"], json!("hi"));
        let mzp = entries[0].data.get("_mzp").expect("_mzp injected");
        assert!(mzp.get("cwd").is_some());
        assert!(mzp.get("pid").is_some());
        assert!(mzp.get("user").is_some());
        assert!(mzp.get("exe").is_some());
    }

    #[tokio::test]
    async fn ingest_batch_ordered() {
        let store = Arc::new(Store::new(1_000_000));
        let app = router(test_state(Arc::clone(&store)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ingest/batch")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "service": "shell",
                            "lines": [
                                "{\"n\":1}",
                                "{\"n\":2}",
                                "plain"
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let (entries, _) = store
            .query_logs(
                Some("shell"),
                None,
                10,
                &CompiledQuery::MatchAll,
                None,
                None,
            )
            .await;
        assert_eq!(entries.len(), 3);
        // Newest first
        assert_eq!(entries[0].data["_raw"], json!("plain"));
        assert_eq!(entries[1].data["n"], json!(2));
        assert_eq!(entries[2].data["n"], json!(1));
        assert!(entries.iter().all(|e| e.data.get("_mzp").is_some()));
    }

    #[tokio::test]
    async fn ingest_batch_injects_cmd() {
        let store = Arc::new(Store::new(1_000_000));
        let app = router(test_state(Arc::clone(&store)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ingest/batch")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "service": "/Users/me/app",
                            "cmd": "npm test",
                            "lines": [
                                "{\"msg\":\"hi\"}",
                                "plain"
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let (entries, _) = store
            .query_logs(
                Some("/Users/me/app"),
                None,
                10,
                &CompiledQuery::MatchAll,
                None,
                None,
            )
            .await;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].data["cmd"], json!("npm test"));
        assert_eq!(entries[0].data["_raw"], json!("plain"));
        assert_eq!(entries[1].data["cmd"], json!("npm test"));
        assert_eq!(entries[1].data["msg"], json!("hi"));
    }

    #[tokio::test]
    async fn ingest_batch_rejects_empty_service() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ingest/batch")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"service":"","lines":["a"]}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn ingest_batch_rejects_oversized() {
        let app = test_app();
        let lines: Vec<String> = (0..129).map(|i| format!("line{i}")).collect();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ingest/batch")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"service":"s","lines": lines}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn investigate_missing_entry_is_404() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/investigate")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"target":"claude","id":999}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // Update checks open real HTTPS sockets (unsupported under Miri).
    #[cfg(not(miri))]
    #[tokio::test]
    async fn get_update_returns_status() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/update")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["installedVersion"], env!("CARGO_PKG_VERSION"));
        assert_eq!(parsed["updateAvailable"], false);
        assert_eq!(parsed["busy"], false);
        assert!(parsed["channel"].is_string());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn post_update_rejects_non_loopback() {
        let app = test_app();
        // No ConnectInfo → PeerAddr defaults to non-loopback → 403
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/update")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn post_update_no_update_is_400() {
        use axum::extract::ConnectInfo;
        use std::net::SocketAddr;

        let app = test_app();
        let mut req = Request::builder()
            .method("POST")
            .uri("/api/update")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 9))));
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn disconnect_blocks_ingest_and_lists_blocked() {
        let store = Arc::new(Store::new(1_000_000));
        let app = router(test_state(Arc::clone(&store)));

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ingest")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"service":"api","line":"{\"msg\":\"hi\"}"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/services/disconnect")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"service":"api"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ingest")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"service":"api","line":"{\"msg\":\"again\"}"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/services")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["services"], json!([]));
        assert_eq!(parsed["blocked"], json!(["api"]));

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/services/reconnect")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"service":"api"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ingest")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"service":"api","line":"{\"msg\":\"back\"}"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(store.stats().await.count, 1);
    }
}
