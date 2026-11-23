CREATE TABLE IF NOT EXISTS rh_message_chunks (
    id              TEXT PRIMARY KEY,
    message_id      TEXT NOT NULL REFERENCES ros_messages(id) ON DELETE CASCADE,
    idx             INTEGER NOT NULL,
    char_start      INTEGER NOT NULL,
    char_end        INTEGER NOT NULL,
    content         TEXT NOT NULL,
    embedding       BLOB,
    embed_status    TEXT,
    embed_error     TEXT,
    embed_failures  INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL,
    owner_user_id   TEXT,
    UNIQUE (message_id, idx)
);
CREATE INDEX IF NOT EXISTS rh_message_chunks_message ON rh_message_chunks (message_id);
CREATE INDEX IF NOT EXISTS rh_message_chunks_unembedded ON rh_message_chunks (created_at)
    WHERE embedding IS NULL AND embed_status IS NULL;
