use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Json};
use axum::routing::{delete, get, post};
use axum::Router;
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::{self, Claims};
use crate::error::{ApiError, ApiResult};
use crate::notifications::truncate;
use crate::AppState;
use riley_comments_core::db;
use riley_comments_core::models::CreateReaction;

pub fn router(_state: Arc<AppState>) -> Router<Arc<AppState>> {
    let public = Router::new()
        .route("/reactions/top", get(top_reactions))
        .route(
            "/comments/{id}/reactions/{emoji}/users",
            get(reaction_users),
        );

    let authed = Router::new()
        .route("/comments/{id}/reactions", post(add_reaction))
        .route(
            "/comments/{id}/reactions/{emoji}",
            delete(remove_reaction),
        )
        .layer(middleware::from_fn(auth::require_auth));

    public.merge(authed)
}

#[derive(Deserialize)]
struct TopParams {
    #[serde(default = "default_top_limit")]
    limit: i64,
}

fn default_top_limit() -> i64 {
    20
}

async fn reaction_users(
    State(state): State<Arc<AppState>>,
    Path((id, emoji)): Path<(Uuid, String)>,
) -> ApiResult<impl IntoResponse> {
    let users = db::reactions::reactors(&state.pool, id, &emoji).await?;
    Ok(Json(users))
}

async fn top_reactions(
    State(state): State<Arc<AppState>>,
    Query(params): Query<TopParams>,
) -> ApiResult<impl IntoResponse> {
    let limit = params.limit.clamp(1, 100);
    let top = db::reactions::top_reactions(&state.pool, limit).await?;
    Ok(Json(top))
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

    db::reactions::add(&state.pool, id, user_id, &claims.username, &input.emoji).await?;

    // Notify comment author about the reaction (unless reacting to your own comment)
    if let Some(notif) = &state.notif {
        if let Ok(comment) = db::comments::get(&state.pool, id).await {
            if comment.user_id != user_id {
                notif.send(
                    comment.user_id,
                    "comment_reaction",
                    &format!("{} reacted {} to your comment", claims.username, input.emoji),
                    &truncate(&comment.body, 200),
                    Some(&format!(
                        "/{}/{}#comment-{}",
                        comment.entity_type, comment.entity_id, comment.id
                    )),
                    Some(serde_json::json!({
                        "comment_id": comment.id,
                        "emoji": input.emoji,
                        "actor_username": claims.username,
                    })),
                );
            }
        }
    }

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
