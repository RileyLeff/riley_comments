use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Json};
use axum::routing::{delete, get, post};
use axum::Router;
use std::sync::Arc;

use crate::auth::{self, Claims};
use crate::error::{ApiError, ApiResult};
use crate::AppState;
use riley_comments_core::db;

pub fn router(_state: Arc<AppState>) -> Router<Arc<AppState>> {
    let public = Router::new()
        .route("/emoji", get(list_emoji));

    let authed = Router::new()
        .route("/emoji", post(upload_emoji))
        .route("/emoji/{name}", delete(delete_emoji))
        .layer(middleware::from_fn(auth::require_auth));

    public.merge(authed)
}

async fn list_emoji(
    State(state): State<Arc<AppState>>,
) -> ApiResult<impl IntoResponse> {
    let emojis = db::custom_emoji::list(&state.pool).await?;
    Ok(Json(emojis))
}

async fn upload_emoji(
    State(state): State<Arc<AppState>>,
    axum::Extension(claims): axum::Extension<Claims>,
    mut multipart: Multipart,
) -> ApiResult<impl IntoResponse> {
    if !claims.is_admin() {
        return Err(ApiError(riley_comments_core::Error::Forbidden(
            "only admins can upload custom emoji".to_string(),
        )));
    }

    let r2 = state.r2.as_ref().ok_or_else(|| {
        ApiError(riley_comments_core::Error::Internal(
            "R2 storage not configured".to_string(),
        ))
    })?;

    let mut name: Option<String> = None;
    let mut file_data: Option<Vec<u8>> = None;
    let mut content_type: Option<String> = None;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        ApiError(riley_comments_core::Error::Validation(format!(
            "multipart error: {e}"
        )))
    })? {
        let field_name = field.name().unwrap_or("").to_string();
        match field_name.as_str() {
            "name" => {
                name = Some(field.text().await.map_err(|e| {
                    ApiError(riley_comments_core::Error::Validation(format!(
                        "failed to read name field: {e}"
                    )))
                })?);
            }
            "file" => {
                content_type = field.content_type().map(|s| s.to_string());
                file_data = Some(field.bytes().await.map_err(|e| {
                    ApiError(riley_comments_core::Error::Validation(format!(
                        "failed to read file: {e}"
                    )))
                })?.to_vec());
            }
            _ => {}
        }
    }

    let name = name
        .filter(|n| !n.trim().is_empty())
        .map(|n| n.trim().to_lowercase())
        .ok_or_else(|| {
            ApiError(riley_comments_core::Error::Validation(
                "name is required".to_string(),
            ))
        })?;

    // Validate name: alphanumeric, hyphens, underscores only
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(ApiError(riley_comments_core::Error::Validation(
            "emoji name must be alphanumeric with hyphens/underscores only".to_string(),
        )));
    }
    if name.len() > 32 {
        return Err(ApiError(riley_comments_core::Error::Validation(
            "emoji name must be 32 characters or fewer".to_string(),
        )));
    }

    let file_data = file_data.ok_or_else(|| {
        ApiError(riley_comments_core::Error::Validation(
            "file is required".to_string(),
        ))
    })?;

    if file_data.len() > 512 * 1024 {
        return Err(ApiError(riley_comments_core::Error::Validation(
            "file must be 512KB or smaller".to_string(),
        )));
    }

    let ct = content_type.as_deref().unwrap_or("image/png");
    let ext = match ct {
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/jpeg" | "image/jpg" => "jpg",
        _ => {
            return Err(ApiError(riley_comments_core::Error::Validation(
                "file must be png, gif, webp, or jpg".to_string(),
            )));
        }
    };

    let key = format!("emoji/{name}.{ext}");

    // Upload to R2
    r2.client
        .put_object()
        .bucket(&r2.bucket)
        .key(&key)
        .body(file_data.into())
        .content_type(ct)
        .cache_control("public, max-age=31536000, immutable")
        .send()
        .await
        .map_err(|e| {
            tracing::error!("R2 upload failed: {e}");
            ApiError(riley_comments_core::Error::Internal(
                "failed to upload image".to_string(),
            ))
        })?;

    let image_url = format!("{}/{}", r2.public_url.trim_end_matches('/'), key);

    let user_id = claims.user_id().map_err(|_| {
        ApiError(riley_comments_core::Error::Internal("bad user id".to_string()))
    })?;

    let emoji = db::custom_emoji::create(&state.pool, &name, &image_url, user_id).await?;

    Ok((StatusCode::CREATED, Json(emoji)))
}

async fn delete_emoji(
    State(state): State<Arc<AppState>>,
    axum::Extension(claims): axum::Extension<Claims>,
    Path(name): Path<String>,
) -> ApiResult<impl IntoResponse> {
    if !claims.is_admin() {
        return Err(ApiError(riley_comments_core::Error::Forbidden(
            "only admins can delete custom emoji".to_string(),
        )));
    }

    let r2 = state.r2.as_ref().ok_or_else(|| {
        ApiError(riley_comments_core::Error::Internal(
            "R2 storage not configured".to_string(),
        ))
    })?;

    // Get the emoji first to find the R2 key
    let emoji = db::custom_emoji::get_by_name(&state.pool, &name).await?;

    // Extract key from URL
    let key = emoji
        .image_url
        .strip_prefix(&format!("{}/", r2.public_url.trim_end_matches('/')))
        .unwrap_or(&emoji.image_url);

    // Delete from R2 (best effort)
    if let Err(e) = r2
        .client
        .delete_object()
        .bucket(&r2.bucket)
        .key(key)
        .send()
        .await
    {
        tracing::warn!("R2 delete failed for {key}: {e}");
    }

    db::custom_emoji::delete_by_name(&state.pool, &name).await?;
    Ok(StatusCode::NO_CONTENT)
}
