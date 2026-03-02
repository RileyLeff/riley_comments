use sqlx::PgPool;
use uuid::Uuid;

use crate::models::*;
use crate::{Error, Result};

/// List comments for an entity, with cursor-based pagination.
/// Returns top-level comments and their replies in chronological order.
pub async fn list(
    pool: &PgPool,
    entity_type: &str,
    entity_id: &str,
    params: &PaginationParams,
) -> Result<PaginatedResponse<CommentWithReactions>> {
    let limit = params.effective_limit();
    let cursor = params.decode_cursor()?;

    // Fetch top-level comments (depth 0) with pagination
    let top_level: Vec<Comment> = if let Some((ts, id)) = cursor {
        sqlx::query_as::<_, Comment>(
            r#"SELECT * FROM comments
               WHERE entity_type = $1 AND entity_id = $2
                 AND depth = 0 AND deleted_at IS NULL
                 AND (created_at, id) > ($3, $4)
               ORDER BY created_at ASC, id ASC
               LIMIT $5"#,
        )
        .bind(entity_type)
        .bind(entity_id)
        .bind(ts)
        .bind(id)
        .bind(limit + 1)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as::<_, Comment>(
            r#"SELECT * FROM comments
               WHERE entity_type = $1 AND entity_id = $2
                 AND depth = 0 AND deleted_at IS NULL
               ORDER BY created_at ASC, id ASC
               LIMIT $3"#,
        )
        .bind(entity_type)
        .bind(entity_id)
        .bind(limit + 1)
        .fetch_all(pool)
        .await?
    };

    let has_more = top_level.len() as i64 > limit;
    let top_level: Vec<Comment> = top_level.into_iter().take(limit as usize).collect();

    let next_cursor = if has_more {
        top_level
            .last()
            .map(|c| encode_cursor(&c.created_at, &c.id))
    } else {
        None
    };

    if top_level.is_empty() {
        return Ok(PaginatedResponse {
            items: vec![],
            next_cursor,
        });
    }

    // Collect all top-level IDs to fetch their replies
    let top_ids: Vec<Uuid> = top_level.iter().map(|c| c.id).collect();

    // Fetch all replies (any depth) for these top-level comments.
    // We use a recursive CTE to get the full thread.
    let replies: Vec<Comment> = sqlx::query_as::<_, Comment>(
        r#"WITH RECURSIVE thread AS (
               SELECT * FROM comments
               WHERE parent_id = ANY($1) AND deleted_at IS NULL
             UNION ALL
               SELECT c.* FROM comments c
               INNER JOIN thread t ON c.parent_id = t.id
               WHERE c.deleted_at IS NULL
           )
           SELECT * FROM thread ORDER BY created_at ASC"#,
    )
    .bind(&top_ids)
    .fetch_all(pool)
    .await?;

    // Collect all comment IDs (top-level + replies) for reaction lookup
    let mut all_comments: Vec<Comment> = top_level;
    all_comments.extend(replies);
    let all_ids: Vec<Uuid> = all_comments.iter().map(|c| c.id).collect();

    // Fetch reactions and reply counts
    let reaction_counts = super::reactions::counts_for_comments(pool, &all_ids).await?;
    let reply_counts = reply_counts(pool, &all_ids).await?;

    // Also include soft-deleted comments that have non-deleted replies
    // (show as "[deleted]" in the UI)
    let deleted_parents: Vec<Comment> = sqlx::query_as::<_, Comment>(
        r#"SELECT DISTINCT c.* FROM comments c
           INNER JOIN comments r ON r.parent_id = c.id AND r.deleted_at IS NULL
           WHERE c.id = ANY($1) AND c.deleted_at IS NOT NULL"#,
    )
    .bind(&all_ids)
    .fetch_all(pool)
    .await?;

    // Build response items
    let mut items: Vec<CommentWithReactions> = all_comments
        .into_iter()
        .map(|c| build_comment_response(c, &reaction_counts, &reply_counts, false))
        .collect();

    for c in deleted_parents {
        items.push(build_comment_response(c, &reaction_counts, &reply_counts, true));
    }

    // Sort by created_at for consistent ordering
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    Ok(PaginatedResponse {
        items,
        next_cursor,
    })
}

/// Get a single comment by ID.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<Comment> {
    sqlx::query_as::<_, Comment>("SELECT * FROM comments WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| Error::NotFound(format!("comment {id} not found")))
}

/// Create a new comment, enforcing max depth.
pub async fn create(
    pool: &PgPool,
    user_id: Uuid,
    username: &str,
    input: &CreateComment,
    max_depth: i32,
) -> Result<Comment> {
    let (parent_id, depth) = if let Some(pid) = input.parent_id {
        let parent = get(pool, pid).await?;
        if parent.deleted_at.is_some() {
            return Err(Error::Validation("cannot reply to a deleted comment".to_string()));
        }
        if parent.entity_type != input.entity_type || parent.entity_id != input.entity_id {
            return Err(Error::Validation("parent belongs to a different entity".to_string()));
        }
        // Enforce max depth: if parent is at max, attach to parent's parent instead
        if parent.depth >= max_depth {
            (parent.parent_id, parent.depth)
        } else {
            (Some(pid), parent.depth + 1)
        }
    } else {
        (None, 0)
    };

    let id = Uuid::now_v7();
    let comment = sqlx::query_as::<_, Comment>(
        r#"INSERT INTO comments (id, parent_id, user_id, username, entity_type, entity_id, body, depth)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
           RETURNING *"#,
    )
    .bind(id)
    .bind(parent_id)
    .bind(user_id)
    .bind(username)
    .bind(&input.entity_type)
    .bind(&input.entity_id)
    .bind(&input.body)
    .bind(depth)
    .fetch_one(pool)
    .await?;

    Ok(comment)
}

/// Update a comment's body. Only the author can edit.
pub async fn update(
    pool: &PgPool,
    id: Uuid,
    user_id: Uuid,
    input: &UpdateComment,
) -> Result<Comment> {
    let comment = get(pool, id).await?;
    if comment.user_id != user_id {
        return Err(Error::Forbidden("you can only edit your own comments".to_string()));
    }
    if comment.deleted_at.is_some() {
        return Err(Error::NotFound(format!("comment {id} not found")));
    }

    let updated = sqlx::query_as::<_, Comment>(
        r#"UPDATE comments SET body = $1, updated_at = now()
           WHERE id = $2 RETURNING *"#,
    )
    .bind(&input.body)
    .bind(id)
    .fetch_one(pool)
    .await?;

    Ok(updated)
}

/// Soft-delete a comment. Author or admin can delete.
pub async fn soft_delete(pool: &PgPool, id: Uuid, user_id: Uuid, is_admin: bool) -> Result<()> {
    let comment = get(pool, id).await?;
    if comment.user_id != user_id && !is_admin {
        return Err(Error::Forbidden(
            "you can only delete your own comments".to_string(),
        ));
    }
    if comment.deleted_at.is_some() {
        return Ok(()); // already deleted
    }

    sqlx::query("UPDATE comments SET deleted_at = now() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;

    Ok(())
}

/// Get reply counts for a set of comment IDs.
async fn reply_counts(
    pool: &PgPool,
    ids: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, i64>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        parent_id: Uuid,
        count: i64,
    }

    let rows: Vec<Row> = sqlx::query_as::<_, Row>(
        r#"SELECT parent_id, COUNT(*) as count
           FROM comments
           WHERE parent_id = ANY($1) AND deleted_at IS NULL
           GROUP BY parent_id"#,
    )
    .bind(ids)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(|r| (r.parent_id, r.count)).collect())
}

fn build_comment_response(
    c: Comment,
    reactions: &std::collections::HashMap<Uuid, Vec<ReactionCount>>,
    reply_counts: &std::collections::HashMap<Uuid, i64>,
    deleted: bool,
) -> CommentWithReactions {
    let is_deleted = deleted || c.deleted_at.is_some();
    CommentWithReactions {
        id: c.id,
        parent_id: c.parent_id,
        user_id: c.user_id,
        username: if is_deleted {
            "[deleted]".to_string()
        } else {
            c.username
        },
        entity_type: c.entity_type,
        entity_id: c.entity_id,
        body: if is_deleted {
            "[deleted]".to_string()
        } else {
            c.body
        },
        depth: c.depth,
        reply_count: reply_counts.get(&c.id).copied().unwrap_or(0),
        reactions: reactions.get(&c.id).cloned().unwrap_or_default(),
        created_at: c.created_at,
        updated_at: c.updated_at,
        deleted: is_deleted,
    }
}
