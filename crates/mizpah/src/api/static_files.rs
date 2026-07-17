//! Embedded SPA static file serving.

use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "static/"]
#[prefix = ""]
struct Assets;

pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match Assets::get(path) {
        Some(file) => {
            let mime = file.metadata.mimetype();
            ([(header::CONTENT_TYPE, mime)], file.data).into_response()
        }
        None => {
            // SPA fallback
            match Assets::get("index.html") {
                Some(file) => {
                    let mime = file.metadata.mimetype();
                    ([(header::CONTENT_TYPE, mime)], file.data).into_response()
                }
                None => (
                    StatusCode::NOT_FOUND,
                    "UI assets not found. Build the web UI and copy into crates/mizpah/static/.",
                )
                    .into_response(),
            }
        }
    }
}
