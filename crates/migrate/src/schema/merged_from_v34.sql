ALTER TABLE rh_topic_redirects ADD COLUMN merged_from TEXT CHECK (merged_from <> '');
