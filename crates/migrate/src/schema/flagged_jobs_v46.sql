CREATE TABLE IF NOT EXISTS rh_job_failures (
    job_key TEXT PRIMARY KEY CHECK (job_key <> ''),
    failures INTEGER NOT NULL CHECK (failures > 0),
    first_failed_at TEXT NOT NULL,
    last_failed_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS rh_flagged_jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task TEXT NOT NULL,
    job_key TEXT UNIQUE,
    payload TEXT NOT NULL,
    priority INTEGER NOT NULL,
    max_attempts INTEGER NOT NULL,
    failures INTEGER NOT NULL CHECK (failures > 0),
    last_error TEXT NOT NULL,
    first_failed_at TEXT NOT NULL,
    last_failed_at TEXT NOT NULL,
    flagged_at TEXT NOT NULL,
    dismissed_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_rh_flagged_jobs_open
    ON rh_flagged_jobs (last_failed_at) WHERE dismissed_at IS NULL;

INSERT INTO rh_flagged_jobs (
    task, job_key, payload, priority, max_attempts, failures, last_error,
    first_failed_at, last_failed_at, flagged_at)
SELECT j.task, NULLIF(j.job_key, ''), j.payload, COALESCE(p.priority, 0), j.max_attempts,
       CASE WHEN j.attempts < 1 THEN 1 ELSE j.attempts END,
       COALESCE(j.last_error, ''), j.updated_at, j.updated_at, j.updated_at
  FROM ros_jobs j
  LEFT JOIN rh_job_priorities p ON p.job_id = j.id
 WHERE j.state = 'dead'
   AND NOT EXISTS (
        SELECT 1 FROM rh_flagged_jobs f
         WHERE f.job_key = NULLIF(j.job_key, '')
   );

INSERT INTO rh_job_failures (job_key, failures, first_failed_at, last_failed_at)
SELECT j.job_key,
       CASE WHEN j.attempts < 1 THEN 1 ELSE j.attempts END,
       j.updated_at, j.updated_at
  FROM ros_jobs j
 WHERE j.state = 'dead' AND NULLIF(j.job_key, '') IS NOT NULL
ON CONFLICT (job_key) DO NOTHING;

DELETE FROM ros_jobs WHERE state = 'dead';
