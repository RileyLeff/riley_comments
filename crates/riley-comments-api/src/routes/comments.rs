use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, patch, post};
use axum::Router;
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::{self, Claims};
use crate::error::{ApiError, ApiResult};
use crate::AppState;
use riley_comments_core::db;
use riley_comments_core::models::*;

pub fn router(_state: Arc<AppState>) -> Router<Arc<AppState>> {
    // Read routes — no auth required
    let public = Router::new()
        .route(
            "/comments/{entity_type}/{entity_id}",
            get(list_comments),
        )
        .route("/comments/{id}", get(get_comment))
        .layer(middleware::from_fn(auth::optional_auth));

    // Write routes — auth required
    let authed = Router::new()
        .route("/comments", post(create_comment))
        .route("/comments/{id}", patch(update_comment))
        .route("/comments/{id}/delete", post(delete_comment))
        .layer(middleware::from_fn(auth::require_auth));

    public.merge(authed)
}

async fn list_comments(
    State(state): State<Arc<AppState>>,
    claims: Option<axum::Extension<Claims>>,
    Path((entity_type, entity_id)): Path<(String, String)>,
    Query(params): Query<PaginationParams>,
) -> ApiResult<impl IntoResponse> {
    let current_user_id = claims
        .and_then(|c| c.0.user_id().ok());
    let page = db::comments::list(&state.pool, &entity_type, &entity_id, &params, current_user_id).await?;
    Ok(Json(page))
}

async fn get_comment(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let comment = db::comments::get(&state.pool, id).await?;
    Ok(Json(comment))
}

async fn create_comment(
    State(state): State<Arc<AppState>>,
    axum::Extension(claims): axum::Extension<Claims>,
    Json(input): Json<CreateComment>,
) -> ApiResult<impl IntoResponse> {
    // Validate body length
    if input.body.trim().is_empty() {
        return Err(ApiError(riley_comments_core::Error::Validation(
            "comment body cannot be empty".to_string(),
        )));
    }
    if input.body.len() > state.config.comments.max_body_length {
        return Err(ApiError(riley_comments_core::Error::Validation(format!(
            "comment body exceeds maximum length of {} characters",
            state.config.comments.max_body_length
        ))));
    }

    let user_id = claims.user_id().map_err(|_| {
        ApiError(riley_comments_core::Error::Internal("bad user id".to_string()))
    })?;

    let comment = db::comments::create(
        &state.pool,
        user_id,
        &claims.username,
        &input,
        state.config.comments.max_depth,
    )
    .await?;

    Ok((StatusCode::CREATED, Json(comment)))
}

async fn update_comment(
    State(state): State<Arc<AppState>>,
    axum::Extension(claims): axum::Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateComment>,
) -> ApiResult<impl IntoResponse> {
    if input.body.trim().is_empty() {
        return Err(ApiError(riley_comments_core::Error::Validation(
            "comment body cannot be empty".to_string(),
        )));
    }
    if input.body.len() > state.config.comments.max_body_length {
        return Err(ApiError(riley_comments_core::Error::Validation(format!(
            "comment body exceeds maximum length of {} characters",
            state.config.comments.max_body_length
        ))));
    }

    let user_id = claims.user_id().map_err(|_| {
        ApiError(riley_comments_core::Error::Internal("bad user id".to_string()))
    })?;

    let comment = db::comments::update(&state.pool, id, user_id, &input).await?;
    Ok(Json(comment))
}

async fn delete_comment(
    State(state): State<Arc<AppState>>,
    axum::Extension(claims): axum::Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let user_id = claims.user_id().map_err(|_| {
        ApiError(riley_comments_core::Error::Internal("bad user id".to_string()))
    })?;

    db::comments::soft_delete(&state.pool, id, user_id, claims.is_admin()).await?;
    Ok(StatusCode::NO_CONTENT)
}
