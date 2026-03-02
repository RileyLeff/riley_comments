use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Json};
use axum::routing::{delete, post};
use axum::Router;
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::{self, Claims};
use crate::error::{ApiError, ApiResult};
use crate::AppState;
use riley_comments_core::db;
use riley_comments_core::models::CreateReaction;

pub fn router(_state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/comments/{id}/reactions",
            post(add_reaction),
        )
        .route(
            "/comments/{id}/reactions/{emoji}",
            delete(remove_reaction),
        )
        .layer(middleware::from_fn(auth::require_auth))
}

async fn add_reaction(
    State(state): State<Arc<AppState>>,
    axum::Extension(claims): axum::Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(input): Json<CreateReaction>,
) -> ApiResult<impl IntoResponse> {
    if input.emoji.is_empty() || input.emoji.len() > 32 {
        return Err(ApiError(riley_comments_core::Error::Validation(
            "invalid emoji".to_string(),
        )));
    }

    let user_id = claims.user_id().map_err(|_| {
        ApiError(riley_comments_core::Error::Internal("bad user id".to_string()))
    })?;

    db::reactions::add(&state.pool, id, user_id, &input.emoji).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_reaction(
    State(state): State<Arc<AppState>>,
    axum::Extension(claims): axum::Extension<Claims>,
    Path((id, emoji)): Path<(Uuid, String)>,
) -> ApiResult<impl IntoResponse> {
    let user_id = claims.user_id().map_err(|_| {
        ApiError(riley_comments_core::Error::Internal("bad user id".to_string()))
    })?;

    db::reactions::remove(&state.pool, id, user_id, &emoji).await?;
    Ok(StatusCode::NO_CONTENT)
}
