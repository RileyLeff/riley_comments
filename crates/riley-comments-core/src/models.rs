use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Database models ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Comment {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub user_id: Uuid,
    pub username: String,
    pub entity_type: String,
    pub entity_id: String,
    pub body: String,
    pub depth: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct CommentReaction {
    pub comment_id: Uuid,
    pub user_id: Uuid,
    pub emoji: String,
    pub created_at: DateTime<Utc>,
}

// ── API request types ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateComment {
    pub entity_type: String,
    pub entity_id: String,
    pub parent_id: Option<Uuid>,
    pub body: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateComment {
    pub body: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateReaction {
    pub emoji: String,
}

// ── API response types ───────────────────────────────────────────────

/// A comment with its aggregated reaction counts.
#[derive(Debug, Clone, Serialize)]
pub struct CommentWithReactions {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub user_id: Uuid,
    pub username: String,
    pub entity_type: String,
    pub entity_id: String,
    pub body: String,
    pub depth: i32,
    pub reply_count: i64,
    pub reactions: Vec<ReactionCount>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReactionCount {
    pub emoji: String,
    pub count: i64,
}

// ── Pagination ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}

impl PaginationParams {
    pub fn effective_limit(&self) -> i64 {
        self.limit.unwrap_or(50).clamp(1, 100)
    }

    pub fn decode_cursor(&self) -> crate::Result<Option<(DateTime<Utc>, Uuid)>> {
        let Some(cursor) = &self.cursor else {
            return Ok(None);
        };
        let parts: Vec<&str> = cursor.splitn(2, ',').collect();
        if parts.len() != 2 {
            return Err(crate::Error::Validation("invalid cursor".to_string()));
        }
        let ts = DateTime::parse_from_rfc3339(parts[0])
            .map_err(|_| crate::Error::Validation("invalid cursor timestamp".to_string()))?
            .with_timezone(&Utc);
        let id = Uuid::parse_str(parts[1])
            .map_err(|_| crate::Error::Validation("invalid cursor id".to_string()))?;
        Ok(Some((ts, id)))
    }
}

pub fn encode_cursor(created_at: &DateTime<Utc>, id: &Uuid) -> String {
    format!("{},{}", created_at.to_rfc3339(), id)
}

#[derive(Debug, Serialize)]
pub struct PaginatedResponse<T: Serialize> {
    pub items: Vec<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}
