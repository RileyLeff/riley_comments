CREATE TABLE comments (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    parent_id   UUID REFERENCES comments(id) ON DELETE CASCADE,
    user_id     UUID NOT NULL,
    username    TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    entity_id   TEXT NOT NULL,
    body        TEXT NOT NULL,
    depth       INTEGER NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at  TIMESTAMPTZ
);

CREATE INDEX idx_comments_entity
    ON comments(entity_type, entity_id, created_at)
    WHERE deleted_at IS NULL;

CREATE INDEX idx_comments_parent
    ON comments(parent_id)
    WHERE deleted_at IS NULL;

CREATE INDEX idx_comments_user
    ON comments(user_id);

CREATE TABLE comment_reactions (
    comment_id UUID NOT NULL REFERENCES comments(id) ON DELETE CASCADE,
    user_id    UUID NOT NULL,
    emoji      TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (comment_id, user_id, emoji)
);
