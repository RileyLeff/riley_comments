-- Thread listing now walks soft-deleted rows too (they are kept as
-- placeholders while live replies remain below them), which the partial
-- "deleted_at IS NULL" indexes from 001 cannot serve.
CREATE INDEX idx_comments_parent_all
    ON comments(parent_id);

CREATE INDEX idx_comments_entity_roots
    ON comments(entity_type, entity_id, created_at, id)
    WHERE depth = 0;
