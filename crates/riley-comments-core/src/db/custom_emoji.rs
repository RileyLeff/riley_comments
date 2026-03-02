use sqlx::PgPool;
use uuid::Uuid;

use crate::models::CustomEmoji;
use crate::{Error, Result};

/// List all custom emojis, ordered by name.
pub async fn list(pool: &PgPool) -> Result<Vec<CustomEmoji>> {
    let emojis = sqlx::query_as::<_, CustomEmoji>(
        "SELECT * FROM custom_emojis ORDER BY name ASC",
    )
    .fetch_all(pool)
    .await?;

    Ok(emojis)
}

/// Get a custom emoji by name.
pub async fn get_by_name(pool: &PgPool, name: &str) -> Result<CustomEmoji> {
    sqlx::query_as::<_, CustomEmoji>(
        "SELECT * FROM custom_emojis WHERE name = $1",
    )
    .bind(name)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| Error::NotFound(format!("custom emoji '{name}' not found")))
}

/// Create a new custom emoji.
pub async fn create(
    pool: &PgPool,
    name: &str,
    image_url: &str,
    created_by: Uuid,
) -> Result<CustomEmoji> {
    let id = Uuid::now_v7();
    let emoji = sqlx::query_as::<_, CustomEmoji>(
        r#"INSERT INTO custom_emojis (id, name, image_url, created_by)
           VALUES ($1, $2, $3, $4)
           RETURNING *"#,
    )
    .bind(id)
    .bind(name)
    .bind(image_url)
    .bind(created_by)
    .fetch_one(pool)
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(ref db_err) = e {
            if db_err.constraint() == Some("custom_emojis_name_key") {
                return Error::Validation(format!("emoji name '{name}' already exists"));
            }
        }
        e.into()
    })?;

    Ok(emoji)
}

/// Delete a custom emoji by name. Returns true if it existed.
pub async fn delete_by_name(pool: &PgPool, name: &str) -> Result<bool> {
    let result = sqlx::query("DELETE FROM custom_emojis WHERE name = $1")
        .bind(name)
        .execute(pool)
        .await?;

    Ok(result.rows_affected() > 0)
}
