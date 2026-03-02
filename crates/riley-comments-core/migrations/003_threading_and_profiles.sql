-- Instagram-style flat threading: track who you're replying to when flattening
ALTER TABLE comments ADD COLUMN reply_to_user_id UUID;
ALTER TABLE comments ADD COLUMN reply_to_username TEXT;

-- Store username on reactions for "who reacted" tooltips
ALTER TABLE comment_reactions ADD COLUMN username TEXT NOT NULL DEFAULT '';

-- Backfill reaction usernames from the most recent comment by each user
UPDATE comment_reactions cr
SET username = COALESCE(
    (SELECT c.username FROM comments c
     WHERE c.user_id = cr.user_id
     ORDER BY c.created_at DESC LIMIT 1),
    ''
);
