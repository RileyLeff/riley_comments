use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::models::*;
use crate::{Error, Result};

/// A top-level comment is listed if it is live, or if it was deleted but
/// still has a live descendant somewhere below it (shown as a placeholder).
const ROOT_IS_VISIBLE: &str = r#"(c.deleted_at IS NULL OR EXISTS (
        WITH RECURSIVE below AS (
            SELECT id, deleted_at FROM comments WHERE parent_id = c.id
          UNION ALL
            SELECT k.id, k.deleted_at FROM comments k
            INNER JOIN below b ON k.parent_id = b.id
        )
        SELECT 1 FROM below WHERE below.deleted_at IS NULL
    ))"#;

/// List comments for an entity, with cursor-based pagination.
/// Returns top-level comments and their replies in chronological order.
/// Deleted comments that still have live replies below them are kept as
/// scrubbed `[deleted]` placeholders so those replies stay attached.
/// If `current_user_id` is provided, reaction responses include `user_reacted`.
pub async fn list(
    pool: &PgPool,
    entity_type: &str,
    entity_id: &str,
    params: &PaginationParams,
    current_user_id: Option<Uuid>,
) -> Result<PaginatedResponse<CommentWithReactions>> {
    let limit = params.effective_limit();
    let cursor = params.decode_cursor()?;

    // Fetch top-level comments (depth 0) with pagination
    let top_level: Vec<Comment> = if let Some((ts, id)) = cursor {
        sqlx::query_as::<_, Comment>(&format!(
            r#"SELECT c.* FROM comments c
               WHERE c.entity_type = $1 AND c.entity_id = $2
                 AND c.depth = 0 AND {ROOT_IS_VISIBLE}
                 AND (c.created_at, c.id) > ($3, $4)
               ORDER BY c.created_at ASC, c.id ASC
               LIMIT $5"#
        ))
        .bind(entity_type)
        .bind(entity_id)
        .bind(ts)
        .bind(id)
        .bind(limit + 1)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as::<_, Comment>(&format!(
            r#"SELECT c.* FROM comments c
               WHERE c.entity_type = $1 AND c.entity_id = $2
                 AND c.depth = 0 AND {ROOT_IS_VISIBLE}
               ORDER BY c.created_at ASC, c.id ASC
               LIMIT $3"#
        ))
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

    // Fetch every reply (any depth, deleted or not) under these top-level
    // comments; deleted ones without live descendants are pruned below.
    let replies: Vec<Comment> = sqlx::query_as::<_, Comment>(
        r#"WITH RECURSIVE thread AS (
               SELECT * FROM comments
               WHERE parent_id = ANY($1)
             UNION ALL
               SELECT c.* FROM comments c
               INNER JOIN thread t ON c.parent_id = t.id
           )
           SELECT * FROM thread ORDER BY created_at ASC, id ASC"#,
    )
    .bind(&top_ids)
    .fetch_all(pool)
    .await?;

    let mut all_comments: Vec<Comment> = top_level;
    all_comments.extend(replies);
    let visible = visible_ids(&all_comments);
    all_comments.retain(|c| visible.contains(&c.id));

    // Collect all comment IDs (top-level + replies) for reaction lookup
    let all_ids: Vec<Uuid> = all_comments.iter().map(|c| c.id).collect();

    // Fetch reactions and reply counts
    let reaction_counts =
        super::reactions::counts_for_comments(pool, &all_ids, current_user_id).await?;
    let reply_counts = reply_counts(pool, &all_ids).await?;

    // Build response items
    let mut items: Vec<CommentWithReactions> = all_comments
        .into_iter()
        .map(|c| build_comment_response(c, &reaction_counts, &reply_counts))
        .collect();

    // Sort by created_at for consistent ordering
    items.sort_by_key(|a| a.created_at);

    Ok(PaginatedResponse { items, next_cursor })
}

/// Ids of the comments to show: every live comment, plus every deleted
/// ancestor of one, so surviving replies keep their place in the thread.
fn visible_ids(comments: &[Comment]) -> HashSet<Uuid> {
    let parents: HashMap<Uuid, Option<Uuid>> =
        comments.iter().map(|c| (c.id, c.parent_id)).collect();
    let mut visible = HashSet::new();
    for live in comments.iter().filter(|c| c.deleted_at.is_none()) {
        let mut next = Some(live.id);
        while let Some(id) = next {
            if !visible.insert(id) {
                break; // this ancestor chain is already marked
            }
            next = parents.get(&id).copied().flatten();
        }
    }
    visible
}

/// Get a single comment by ID.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<Comment> {
    sqlx::query_as::<_, Comment>("SELECT * FROM comments WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| Error::NotFound(format!("comment {id} not found")))
}

/// Get a single comment for public readers. Soft-deleted comments are
/// reported as not found so their body and author stay hidden.
pub async fn get_visible(pool: &PgPool, id: Uuid) -> Result<Comment> {
    sqlx::query_as::<_, Comment>("SELECT * FROM comments WHERE id = $1 AND deleted_at IS NULL")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| Error::NotFound(format!("comment {id} not found")))
}

/// Create a new comment, enforcing max depth.
/// When replying to a reply (depth >= max_depth), the comment is flattened to the
/// parent level and `reply_to_user_id`/`reply_to_username` record the intended target.
pub async fn create(
    pool: &PgPool,
    user_id: Uuid,
    username: &str,
    input: &CreateComment,
    max_depth: i32,
) -> Result<Comment> {
    let (parent_id, depth, reply_to_user_id, reply_to_username) = if let Some(pid) = input.parent_id
    {
        let parent = get(pool, pid).await?;
        if parent.deleted_at.is_some() {
            return Err(Error::Validation(
                "cannot reply to a deleted comment".to_string(),
            ));
        }
        if parent.entity_type != input.entity_type || parent.entity_id != input.entity_id {
            return Err(Error::Validation(
                "parent belongs to a different entity".to_string(),
            ));
        }
        // Enforce max depth: if parent is at max, flatten to parent's parent
        // and record who we're actually replying to
        if parent.depth >= max_depth {
            (
                parent.parent_id,
                parent.depth,
                Some(parent.user_id),
                Some(parent.username.clone()),
            )
        } else {
            (Some(pid), parent.depth + 1, None, None)
        }
    } else {
        (None, 0, None, None)
    };

    let id = Uuid::now_v7();
    let comment = sqlx::query_as::<_, Comment>(
        r#"INSERT INTO comments (id, parent_id, user_id, username, entity_type, entity_id, body, depth, reply_to_user_id, reply_to_username)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
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
    .bind(reply_to_user_id)
    .bind(reply_to_username)
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
        return Err(Error::Forbidden(
            "you can only edit your own comments".to_string(),
        ));
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
async fn reply_counts(pool: &PgPool, ids: &[Uuid]) -> Result<std::collections::HashMap<Uuid, i64>> {
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

/// Deleted comments become scrubbed placeholders: no body, author, or
/// reactions, matching how the site renders `deleted: true`.
fn build_comment_response(
    c: Comment,
    reactions: &HashMap<Uuid, Vec<ReactionCount>>,
    reply_counts: &HashMap<Uuid, i64>,
) -> CommentWithReactions {
    let reply_count = reply_counts.get(&c.id).copied().unwrap_or(0);
    if c.deleted_at.is_some() {
        return CommentWithReactions {
            id: c.id,
            parent_id: c.parent_id,
            user_id: Uuid::nil(),
            username: "[deleted]".to_string(),
            entity_type: c.entity_type,
            entity_id: c.entity_id,
            body: "[deleted]".to_string(),
            depth: c.depth,
            reply_to_user_id: None,
            reply_to_username: None,
            reply_count,
            reactions: vec![],
            created_at: c.created_at,
            updated_at: c.created_at,
            deleted: true,
        };
    }
    CommentWithReactions {
        id: c.id,
        parent_id: c.parent_id,
        user_id: c.user_id,
        username: c.username,
        entity_type: c.entity_type,
        entity_id: c.entity_id,
        body: c.body,
        depth: c.depth,
        reply_to_user_id: c.reply_to_user_id,
        reply_to_username: c.reply_to_username,
        reply_count,
        reactions: reactions.get(&c.id).cloned().unwrap_or_default(),
        created_at: c.created_at,
        updated_at: c.updated_at,
        deleted: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn comment(id: u128, parent: Option<u128>, deleted: bool) -> Comment {
        Comment {
            id: Uuid::from_u128(id),
            parent_id: parent.map(Uuid::from_u128),
            user_id: Uuid::nil(),
            username: String::new(),
            entity_type: String::new(),
            entity_id: String::new(),
            body: String::new(),
            depth: 0,
            reply_to_user_id: None,
            reply_to_username: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            deleted_at: deleted.then(Utc::now),
        }
    }

    #[test]
    fn deleted_ancestors_of_live_comments_stay_visible() {
        // 1(del) -> 2(del) -> 3(live); 1 -> 4(del); 5(del) -> 6(del); 7(live)
        let comments = vec![
            comment(1, None, true),
            comment(2, Some(1), true),
            comment(3, Some(2), false),
            comment(4, Some(1), true),
            comment(5, None, true),
            comment(6, Some(5), true),
            comment(7, None, false),
        ];
        let mut visible: Vec<u128> = visible_ids(&comments)
            .into_iter()
            .map(|id| id.as_u128())
            .collect();
        visible.sort();
        assert_eq!(visible, vec![1, 2, 3, 7]);
    }
}
