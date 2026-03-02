use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use riley_comments_core::Error as CoreError;

pub struct ApiError(pub CoreError);

impl From<CoreError> for ApiError {
    fn from(e: CoreError) -> Self {
        Self(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match &self.0 {
            CoreError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            CoreError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg.clone()),
            CoreError::Validation(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            CoreError::Config(msg) => {
                tracing::error!("config error: {msg}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
            CoreError::Database(e) => {
                tracing::error!("database error: {e}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
            CoreError::Migration(e) => {
                tracing::error!("migration error: {e}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
            CoreError::Internal(msg) => {
                tracing::error!("internal error: {msg}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
        };

        (status, Json(serde_json::json!({"error": message}))).into_response()
    }
}

pub type ApiResult<T> = std::result::Result<T, ApiError>;
