use sqlx::PgPool;
use uuid::Uuid;

use crate::models::UserCard;
use crate::{Error, Result};

/// Get a user's profile card with Riley Points.
/// Riley Points = comments posted + reactions received on their comments.
pub async fn get_card(pool: &PgPool, user_id: Uuid) -> Result<UserCard> {
    #[derive(sqlx::FromRow)]
    struct Row {
        username: String,
        comment_count: i64,
        first_seen: chrono::DateTime<chrono::Utc>,
    }

    let row = sqlx::query_as::<_, Row>(
        r#"SELECT
               (SELECT username FROM comments WHERE user_id = $1 AND deleted_at IS NULL ORDER BY created_at DESC LIMIT 1) as username,
               (SELECT COUNT(*) FROM comments WHERE user_id = $1 AND deleted_at IS NULL) as comment_count,
               (SELECT MIN(created_at) FROM comments WHERE user_id = $1) as first_seen"#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Err(Error::NotFound(format!("user {user_id} not found")));
    };

    // username could be NULL if no non-deleted comments exist
    if row.username.is_empty() {
        return Err(Error::NotFound(format!("user {user_id} not found")));
    }

    // Count reactions received on this user's comments
    let reactions_received: (i64,) = sqlx::query_as(
        r#"SELECT COUNT(*) FROM comment_reactions cr
           INNER JOIN comments c ON c.id = cr.comment_id
           WHERE c.user_id = $1 AND c.deleted_at IS NULL"#,
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;

    let comment_count = row.comment_count;
    let reactions_received = reactions_received.0;

    Ok(UserCard {
        user_id,
        username: row.username,
        comment_count,
        reactions_received,
        riley_points: comment_count + reactions_received,
        first_seen: row.first_seen,
    })
}
