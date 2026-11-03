pub(crate) const CLAIM: &str = "WITH available AS MATERIALIZED (
    SELECT j.id, j.job_queue_id, j.priority, j.run_at FROM graphile_worker._private_jobs j
    JOIN graphile_worker._private_tasks t ON t.id = j.task_id
    WHERE t.identifier = ANY($1)
      AND ($2::bigint IS NULL OR j.id = $2)
      AND ($4::smallint IS NULL OR j.priority <= $4)
      AND j.is_available = true
      AND (j.run_at <= now() OR $2::bigint IS NOT NULL)
      AND (j.job_queue_id IS NULL OR EXISTS (
          SELECT 1 FROM graphile_worker._private_job_queues q
          WHERE q.id = j.job_queue_id
            AND (q.locked_at IS NULL OR q.locked_at < now() - interval '4 hours')
          FOR UPDATE SKIP LOCKED
      ))
    ORDER BY j.priority, j.run_at, j.id
    LIMIT 1 FOR UPDATE OF j SKIP LOCKED
), stale AS MATERIALIZED (
    SELECT j.id, j.job_queue_id, j.priority, j.run_at FROM graphile_worker._private_jobs j
    JOIN graphile_worker._private_tasks t ON t.id = j.task_id
    WHERE t.identifier = ANY($1)
      AND ($2::bigint IS NULL OR j.id = $2)
      AND ($4::smallint IS NULL OR j.priority <= $4)
      AND j.locked_at < now() - interval '4 hours'
      AND j.attempts < j.max_attempts
      AND j.run_at <= now()
      AND (j.job_queue_id IS NULL OR EXISTS (
          SELECT 1 FROM graphile_worker._private_job_queues q
          WHERE q.id = j.job_queue_id
            AND (q.locked_at IS NULL OR q.locked_at < now() - interval '4 hours')
          FOR UPDATE SKIP LOCKED
      ))
    ORDER BY j.priority, j.run_at, j.id
    LIMIT 1 FOR UPDATE OF j SKIP LOCKED
), selected AS MATERIALIZED (
    SELECT candidate.id, candidate.job_queue_id
    FROM (SELECT * FROM available UNION ALL SELECT * FROM stale) candidate
    ORDER BY candidate.priority, candidate.run_at, candidate.id
    LIMIT 1
), locked_queue AS (
    UPDATE graphile_worker._private_job_queues q
    SET locked_at = now(), locked_by = $3
    FROM selected s WHERE q.id = s.job_queue_id
    RETURNING q.id
)
UPDATE graphile_worker._private_jobs j
SET attempts = j.attempts + 1, locked_at = now(), locked_by = $3, updated_at = now()
FROM selected s, graphile_worker._private_tasks t
WHERE j.id = s.id AND t.id = j.task_id
  AND (s.job_queue_id IS NULL OR s.job_queue_id IN (SELECT id FROM locked_queue))
RETURNING j.id, t.identifier, j.payload, j.attempts::bigint";

pub(crate) const COMPLETE: &str = "WITH removed AS (
    DELETE FROM graphile_worker._private_jobs
    WHERE id = $1 AND starts_with(locked_by, $2)
    RETURNING id, key, job_queue_id, locked_by, task_id
), cleared AS (
    DELETE FROM rh_job_failures c USING removed r WHERE c.job_key = coalesce(r.key, 'job:' || r.id)
), resolved AS (
    DELETE FROM rh_flagged_jobs f USING removed r WHERE f.job_key = r.key
), unlocked AS (
    UPDATE graphile_worker._private_job_queues q
    SET locked_at = NULL, locked_by = NULL
    FROM removed r WHERE q.id = r.job_queue_id AND q.locked_by = r.locked_by
), finished AS (
    INSERT INTO rh_worker_runs (task, finished_at)
    SELECT t.identifier, now() FROM removed r JOIN graphile_worker._private_tasks t ON t.id = r.task_id
    ON CONFLICT (task) DO UPDATE SET finished_at = EXCLUDED.finished_at
)
SELECT count(*)::bigint AS removed FROM removed";

pub(crate) const ARM: &str = "WITH kept AS (
    UPDATE graphile_worker._private_jobs j
    SET priority = LEAST(j.priority, $6::smallint),
        payload = CASE WHEN $6::smallint < j.priority THEN $2::json ELSE j.payload END,
        updated_at = now()
    WHERE j.key = $5 AND j.locked_at IS NULL AND j.attempts < j.max_attempts
    RETURNING j.id
)
SELECT id FROM kept
UNION ALL
SELECT (graphile_worker.add_job($1, $2::json, $7, $3, $4, $5, $6, null::text[], 'replace')).id
WHERE NOT EXISTS (SELECT 1 FROM kept)";

pub(crate) const RELEASE: &str = "WITH owned AS MATERIALIZED (
    SELECT id, job_queue_id, locked_by FROM graphile_worker._private_jobs
    WHERE id = $1 AND starts_with(locked_by, $2)
    FOR UPDATE
), released AS (
    UPDATE graphile_worker._private_jobs j
    SET attempts = GREATEST(j.attempts - 1, 0),
        locked_at = NULL, locked_by = NULL, updated_at = now(),
        run_at = coalesce(now() + make_interval(secs => $3::float8), j.run_at)
    FROM owned WHERE j.id = owned.id
    RETURNING owned.job_queue_id, owned.locked_by
), unlocked AS (
    UPDATE graphile_worker._private_job_queues q
    SET locked_at = NULL, locked_by = NULL
    FROM released r WHERE q.id = r.job_queue_id AND q.locked_by = r.locked_by
)
SELECT count(*)::bigint AS released FROM released";

pub(crate) const FAIL: &str = "WITH owned AS MATERIALIZED (
    SELECT id, job_queue_id, locked_by FROM graphile_worker._private_jobs
    WHERE id = $1 AND starts_with(locked_by, $3)
    FOR UPDATE
), failed AS (
    UPDATE graphile_worker._private_jobs j
    SET last_error = $2, locked_at = NULL, locked_by = NULL, updated_at = now(),
        run_at = greatest(now(), j.run_at) + interval '1 second' * exp(least(j.attempts, 10))
    FROM owned WHERE j.id = owned.id
    RETURNING owned.job_queue_id, owned.locked_by
), unlocked AS (
    UPDATE graphile_worker._private_job_queues q
    SET locked_at = NULL, locked_by = NULL
    FROM failed f WHERE q.id = f.job_queue_id AND q.locked_by = f.locked_by
)
SELECT count(*)::bigint AS failed FROM failed";
