//! Typed errors for Mizpah subsystems. Map to strings / HTTP at CLI and API edges.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use thiserror::Error;

/// CEL filter compile / bind failures.
#[derive(Debug, Error)]
pub enum FilterError {
    #[error("invalid CEL query: {0}")]
    Compile(String),
    #[error("bind {key}: {message}")]
    Bind { key: String, message: String },
}

/// Hub process lifecycle / bind failures (start, stop, PID file, spawn).
#[derive(Debug, Error)]
pub enum HubLifecycleError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Message(String),
}

impl HubLifecycleError {
    pub fn msg(s: impl Into<String>) -> Self {
        Self::Message(s.into())
    }
}

/// Axum handler errors mapped to HTTP status + message body.
#[derive(Debug, Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    BadGateway(String),
    #[error("{0}")]
    Internal(String),
}

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Conflict(msg.into())
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::Forbidden(msg.into())
    }

    pub fn bad_gateway(msg: impl Into<String>) -> Self {
        Self::BadGateway(msg.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::BadGateway(_) => StatusCode::BAD_GATEWAY,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status(), self.to_string()).into_response()
    }
}

impl From<FilterError> for ApiError {
    fn from(err: FilterError) -> Self {
        Self::BadRequest(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    #[test]
    fn hub_lifecycle_msg() {
        let err = HubLifecycleError::msg("boom");
        assert_eq!(err.to_string(), "boom");
    }

    #[test]
    fn api_error_constructors_and_status() {
        assert!(matches!(ApiError::bad_request("x"), ApiError::BadRequest(_)));
        assert!(matches!(ApiError::not_found("x"), ApiError::NotFound(_)));
        assert!(matches!(ApiError::conflict("x"), ApiError::Conflict(_)));
        assert!(matches!(ApiError::forbidden("x"), ApiError::Forbidden(_)));
        assert!(matches!(ApiError::bad_gateway("x"), ApiError::BadGateway(_)));
        assert!(matches!(ApiError::internal("x"), ApiError::Internal(_)));

        assert_eq!(ApiError::BadGateway("x".into()).into_response().status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            ApiError::Internal("x".into()).into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn filter_error_maps_to_bad_request_response() {
        let api: ApiError = FilterError::Compile("bad cel".into()).into();
        let resp = api.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn filter_error_bind_and_io() {
        let bind = FilterError::Bind {
            key: "x".into(),
            message: "bad".into(),
        };
        assert!(bind.to_string().contains("x"));
        let io: HubLifecycleError = std::io::Error::new(std::io::ErrorKind::NotFound, "nope").into();
        assert!(io.to_string().contains("nope"));
    }

    #[test]
    fn api_error_into_response_all_statuses() {
        assert_eq!(
            ApiError::BadRequest("bad".into()).into_response().status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ApiError::Forbidden("no".into()).into_response().status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            ApiError::Conflict("c".into()).into_response().status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            ApiError::NotFound("n".into()).into_response().status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            ApiError::BadGateway("gw".into()).into_response().status(),
            StatusCode::BAD_GATEWAY
        );
    }

    #[test]
    fn filter_error_compile_display() {
        let err = FilterError::Compile("syntax".into());
        assert_eq!(err.to_string(), "invalid CEL query: syntax");
    }

    #[test]
    fn hub_lifecycle_io_from_error() {
        let err: HubLifecycleError =
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied").into();
        assert!(err.to_string().contains("denied"));
    }
}
