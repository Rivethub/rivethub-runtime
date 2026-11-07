CREATE TABLE IF NOT EXISTS rh_daily_digests (
    owner_user_id TEXT NOT NULL CHECK (owner_user_id <> ''),
    day TEXT NOT NULL CHECK (length(day) = 10),
    summary TEXT NOT NULL CHECK (summary <> ''),
    changes INTEGER NOT NULL CHECK (changes >= 0),
    written_at TEXT NOT NULL,
    PRIMARY KEY (owner_user_id, day)
);
