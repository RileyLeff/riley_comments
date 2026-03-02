use sqlx::PgPool;
use uuid::Uuid;

use crate::models::*;
use crate::{Error, Result};

/// Add a reaction to a comment. Idempotent — re-adding the same emoji is a no-op.
pub async fn add(pool: &PgPool, comment_id: Uuid, user_id: Uuid, emoji: &str) -> Result<()> {
    // Verify the comment exists and isn't deleted
    let exists: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM comments WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(comment_id)
    .fetch_optional(pool)
    .await?;

    if exists.is_none() {
        return Err(Error::NotFound(format!("comment {comment_id} not found")));
    }

    sqlx::query(
        r#"INSERT INTO comment_reactions (comment_id, user_id, emoji)
           VALUES ($1, $2, $3)
           ON CONFLICT (comment_id, user_id, emoji) DO NOTHING"#,
    )
    .bind(comment_id)
    .bind(user_id)
    .bind(emoji)
    .execute(pool)
    .await?;

    Ok(())
}

/// Remove a reaction. No-op if it doesn't exist.
pub async fn remove(pool: &PgPool, comment_id: Uuid, user_id: Uuid, emoji: &str) -> Result<()> {
    sqlx::query(
        "DELETE FROM comment_reactions WHERE comment_id = $1 AND user_id = $2 AND emoji = $3",
    )
    .bind(comment_id)
    .bind(user_id)
    .bind(emoji)
    .execute(pool)
    .await?;

    Ok(())
}

/// Get aggregated reaction counts for a set of comments.
pub async fn counts_for_comments(
    pool: &PgPool,
    comment_ids: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, Vec<ReactionCount>>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        comment_id: Uuid,
        emoji: String,
        count: i64,
    }

    let rows: Vec<Row> = sqlx::query_as::<_, Row>(
        r#"SELECT comment_id, emoji, COUNT(*) as count
           FROM comment_reactions
           WHERE comment_id = ANY($1)
           GROUP BY comment_id, emoji
           ORDER BY count DESC"#,
    )
    .bind(comment_ids)
    .fetch_all(pool)
    .await?;

    let mut map: std::collections::HashMap<Uuid, Vec<ReactionCount>> =
        std::collections::HashMap::new();
    for row in rows {
        map.entry(row.comment_id)
            .or_default()
            .push(ReactionCount {
                emoji: row.emoji,
                count: row.count,
            });
    }

    Ok(map)
}
