use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use std::sync::Arc;
use uuid::Uuid;

use crate::error::ApiResult;
use crate::AppState;
use riley_comments_core::db;

pub fn router(_state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new().route("/users/{id}/card", get(user_card))
}

async fn user_card(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let card = db::users::get_card(&state.pool, id).await?;
    Ok(Json(card))
}
