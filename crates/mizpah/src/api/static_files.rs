//! Embedded SPA static file serving.

use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "static/"]
#[prefix = ""]
struct Assets;

pub(crate) struct StaticAsset {
    mime: String,
    data: Vec<u8>,
}

fn asset_response(file: StaticAsset) -> Response {
    let mut res = file.data.into_response();
    if let Ok(val) = HeaderValue::from_str(&file.mime) {
        res.headers_mut().insert(header::CONTENT_TYPE, val);
    }
    res
}

/// Resolve a URL path to a static response using `get_asset` for lookups.
pub(crate) fn resolve_static_response(
    path: &str,
    get_asset: impl Fn(&str) -> Option<StaticAsset>,
) -> Response {
    let path = path.trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match get_asset(path) {
        Some(file) => asset_response(file),
        None => match get_asset("index.html") {
            Some(file) => asset_response(file),
            None => (
                StatusCode::NOT_FOUND,
                "UI assets not found. Build the web UI and copy into crates/mizpah/static/.",
            )
                .into_response(),
        },
    }
}

fn embedded_asset(path: &str) -> Option<StaticAsset> {
    Assets::get(path).map(|file| StaticAsset {
        mime: file.metadata.mimetype().to_string(),
        data: file.data.into_owned(),
    })
}

pub async fn static_handler(uri: Uri) -> Response {
    resolve_static_response(uri.path(), embedded_asset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{self, AppState};
    use crate::store::Store;
    use crate::update;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use std::sync::Arc;
    use tower::ServiceExt;

    fn test_app() -> axum::Router {
        api::router(AppState {
            store: Arc::new(Store::new(1_000_000)),
            project_dir: std::env::temp_dir(),
            update: update::UpdateManager::new(update::RestartContext {
                host: "127.0.0.1".into(),
                port: 3149,
                project_dir: std::env::temp_dir(),
                max_bytes: 1_000_000,
                ttl_hours: 0,
            }),
            auth: None,
        })
    }

    // Full axum oneshot + embedded assets is extremely slow under Miri; resolve_*
    // unit tests cover the same routing logic without the service stack.
    #[cfg(not(miri))]
    #[tokio::test]
    async fn serves_index_at_root() {
        let app = test_app();
        let resp = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/html")
        );
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn serves_embedded_asset() {
        let js_path = Assets::iter()
            .find(|p| p.ends_with(".js"))
            .expect("embedded JS asset");
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/{js_path}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.contains("javascript") || ct.contains("ecmascript") || ct.contains("js"),
            "unexpected content-type {ct:?} for {js_path}"
        );
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn spa_fallback_for_missing_path() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/app/deep/route")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/html")
        );
    }

    #[test]
    fn missing_assets_returns_404_message() {
        let resp = resolve_static_response("/missing", |_| None);
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn resolve_direct_asset_hit() {
        let resp = resolve_static_response("index.html", |path| {
            if path == "index.html" {
                Some(StaticAsset {
                    mime: "text/html".into(),
                    data: b"<html></html>".to_vec(),
                })
            } else {
                None
            }
        });
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn resolve_spa_fallback() {
        let resp = resolve_static_response("app/route", |path| {
            if path == "index.html" {
                Some(StaticAsset {
                    mime: "text/html".into(),
                    data: b"<html></html>".to_vec(),
                })
            } else {
                None
            }
        });
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn resolve_empty_path_to_index() {
        let resp = resolve_static_response("", |path| {
            if path == "index.html" {
                Some(StaticAsset {
                    mime: "text/html".into(),
                    data: b"<html/>".to_vec(),
                })
            } else {
                None
            }
        });
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn asset_response_skips_invalid_content_type() {
        let resp = asset_response(StaticAsset {
            mime: "\n".into(), // HeaderValue::from_str rejects control chars
            data: b"hi".to_vec(),
        });
        assert_eq!(resp.status(), StatusCode::OK);
        // Invalid mime must not be applied; body response may still set a default type.
        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(!ct.contains('\n'));
        assert_ne!(ct, "\n");
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn static_handler_serves_embedded_assets() {
        let resp = static_handler(Uri::from_static("/")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/html")
        );
    }
}
