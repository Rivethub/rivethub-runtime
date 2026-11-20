-- Existing pooled embeddings with no specification cannot be verified.
UPDATE ros_summaries
SET embedding = NULL, index_needs_repair = 1
WHERE kind IN ('session', 'part') AND (index_key IS NULL OR embedding IS NULL);
