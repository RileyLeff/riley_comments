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
/// If `current_user_id` is provided, each reaction includes whether that user reacted.
pub async fn counts_for_comments(
    pool: &PgPool,
    comment_ids: &[Uuid],
    current_user_id: Option<Uuid>,
) -> Result<std::collections::HashMap<Uuid, Vec<ReactionCount>>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        comment_id: Uuid,
        emoji: String,
        count: i64,
        user_reacted: bool,
    }

    // Use nil UUID as sentinel when no user — never matches a real user_id
    let uid = current_user_id.unwrap_or(Uuid::nil());

    let rows: Vec<Row> = sqlx::query_as::<_, Row>(
        r#"SELECT comment_id, emoji, COUNT(*) as count,
                  BOOL_OR(user_id = $2) as user_reacted
           FROM comment_reactions
           WHERE comment_id = ANY($1)
           GROUP BY comment_id, emoji
           ORDER BY count DESC"#,
    )
    .bind(comment_ids)
    .bind(uid)
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
                user_reacted: row.user_reacted,
            });
    }

    Ok(map)
}

/// Get the top N most-used reaction emoji across all comments.
pub async fn top_reactions(pool: &PgPool, limit: i64) -> Result<Vec<ReactionCount>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        emoji: String,
        total: i64,
    }

    let rows: Vec<Row> = sqlx::query_as::<_, Row>(
        r#"SELECT emoji, COUNT(*) as total
           FROM comment_reactions
           GROUP BY emoji
           ORDER BY total DESC
           LIMIT $1"#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| ReactionCount {
            emoji: r.emoji,
            count: r.total,
            user_reacted: false,
        })
        .collect())
}
