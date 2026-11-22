CREATE TABLE IF NOT EXISTS ros_summary_passages (
    fts_rowid INTEGER PRIMARY KEY,
    id TEXT NOT NULL UNIQUE,
    record_id TEXT NOT NULL REFERENCES ros_summaries(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    char_start INTEGER NOT NULL CHECK (char_start >= 0),
    char_end INTEGER NOT NULL,
    content TEXT NOT NULL CHECK (length(content) BETWEEN 1 AND 6000),
    earliest_at TEXT NOT NULL,
    latest_at TEXT NOT NULL,
    index_key BLOB NOT NULL CHECK (length(index_key) = 32),
    embedding BLOB CHECK (length(embedding) = 4096),
    CHECK (char_end - char_start = length(content)),
    UNIQUE (record_id, ordinal)
);

CREATE VIRTUAL TABLE IF NOT EXISTS ros_summary_passages_fts USING fts5(content, tokenize='porter unicode61');

CREATE TRIGGER IF NOT EXISTS ros_summary_passages_insert AFTER INSERT ON ros_summary_passages
BEGIN
    INSERT INTO ros_summary_passages_fts (rowid, content) VALUES (new.rowid, new.content);
    UPDATE ros_summaries SET index_needs_repair = 1 WHERE id = new.record_id;
END;

CREATE TRIGGER IF NOT EXISTS ros_summary_passages_delete AFTER DELETE ON ros_summary_passages
BEGIN
    DELETE FROM ros_summary_passages_fts WHERE rowid = old.rowid;
    UPDATE ros_summaries SET index_needs_repair = 1 WHERE id = old.record_id;
END;

CREATE TRIGGER IF NOT EXISTS ros_summary_passages_text AFTER UPDATE OF content ON ros_summary_passages
BEGIN
    DELETE FROM ros_summary_passages_fts WHERE rowid = old.rowid;
    INSERT INTO ros_summary_passages_fts (rowid, content) VALUES (new.rowid, new.content);
END;

CREATE TRIGGER IF NOT EXISTS ros_summary_passages_update AFTER UPDATE ON ros_summary_passages
BEGIN
    UPDATE ros_summaries SET index_needs_repair = 1 WHERE id IN (old.record_id, new.record_id);
END;

CREATE TRIGGER IF NOT EXISTS ros_summary_passages_invalidate AFTER UPDATE OF content, index_key ON ros_summaries
WHEN new.kind IN ('session', 'part') AND (new.content IS NOT old.content OR new.index_key IS NOT old.index_key)
BEGIN
    UPDATE ros_summaries SET index_needs_repair = 1 WHERE id = new.id;
END;

CREATE VIEW IF NOT EXISTS ros_summary_records_indexed AS
SELECT s.id
FROM ros_summaries s JOIN ros_summary_passages p ON p.record_id = s.id
LEFT JOIN ros_summary_passages prev ON prev.record_id = p.record_id AND prev.ordinal = p.ordinal - 1
WHERE s.kind IN ('session', 'part') AND s.index_key IS NOT NULL
GROUP BY s.id
HAVING max(p.char_end) = length(s.content)
    AND min(p.embedding IS NOT NULL AND p.index_key = s.index_key
        AND p.content = substr(s.content, p.char_start + 1, p.char_end - p.char_start)
        AND (CASE WHEN p.ordinal = 0 THEN 0 ELSE coalesce(prev.char_end, -1) END) = p.char_start) = 1;

UPDATE ros_summaries SET embedding = NULL, index_needs_repair = 1
WHERE kind IN ('session', 'part') AND id NOT IN (SELECT id FROM ros_summary_records_indexed);
