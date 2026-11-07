CREATE TABLE IF NOT EXISTS rh_review_marks (
    owner_user_id TEXT NOT NULL CHECK (owner_user_id <> ''),
    item_key TEXT NOT NULL CHECK (item_key <> '' AND length(item_key) <= 512),
    state TEXT NOT NULL CHECK (state IN ('decided', 'snoozed')),
    at TEXT NOT NULL,
    PRIMARY KEY (owner_user_id, item_key)
);
