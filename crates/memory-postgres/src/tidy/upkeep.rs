use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::filing::LeftOut;
use kernel::filing::bare_session_prefix;
use kernel::tidy::FilingTally;
use kernel::tidy::TaskActivity;
use kernel::tidy::TidyError;
use kernel::tidy::TypeTally;
use kernel::tidy::UpkeepStore;
use kernel::tidy::WikiTally;
use kernel::topics::TopicId;
use kernel::topics::TopicName;

use super::count;
use super::failed;
use crate::PostgresMemory;
use crate::graphile::GraphileQueue;

#[derive(sqlx::FromRow)]
struct FailingRow {
    task: String,
    failing: i64,
}

#[async_trait]
impl UpkeepStore for PostgresMemory {
    async fn filing(&self, since: DateTime<Utc>) -> Result<FilingTally, TidyError> {
        let left_out = LeftOut::rules();
        let row = sqlx::query!(
            r#"SELECT count(*) FILTER (WHERE f.left_out) AS "left_out!",
                      count(*) FILTER (WHERE NOT f.left_out AND f.placed_by = 'categorizer') AS "categorizer!",
                      count(*) FILTER (WHERE NOT f.left_out AND f.placed_by = 'you') AS "you!",
                      count(*) FILTER (WHERE NOT f.left_out AND f.placed_by IS NULL) AS "uncategorized!",
                      (SELECT count(*) FROM rh_filing_corrections WHERE corrected_at >= $4) AS "corrections!"
                 FROM (SELECT (c.channel = ANY($2)
                               OR EXISTS (SELECT 1 FROM unnest($1::text[]) AS prefix
                                           WHERE starts_with(c.session_key, prefix))) AS left_out,
                              coalesce(cp.placed_by, bare.placed_by, prefixed.placed_by) AS placed_by
                         FROM rh_conversations c
                         LEFT JOIN rh_conversation_placements cp ON cp.conversation_id = c.id
                         LEFT JOIN rh_session_placements bare ON bare.session_id = c.session_key
                         LEFT JOIN rh_session_placements prefixed
                                ON prefixed.session_id = $3 || c.session_key
                       OFFSET 0) f"#,
            &left_out.key_prefixes,
            &left_out.channels,
            bare_session_prefix(),
            since
        )
        .fetch_one(&self.pool)
        .await
        .map_err(failed("filing upkeep"))?;
        Ok(FilingTally {
            categorizer: count(row.categorizer)?,
            you: count(row.you)?,
            uncategorized: count(row.uncategorized)?,
            left_out: count(row.left_out)?,
            corrections: count(row.corrections)?,
        })
    }

    async fn wiki(&self) -> Result<WikiTally, TidyError> {
        let pages = sqlx::query!(
            r#"SELECT count(*) FILTER (WHERE t.kind = 'work') AS "topic_pages!",
                      count(*) FILTER (WHERE w.topic_id IS NULL) AS "loose!"
                 FROM rh_wiki_topics w
                 LEFT JOIN rh_topics t ON t.id = w.topic_id"#
        )
        .fetch_one(&self.pool)
        .await
        .map_err(failed("wiki upkeep"))?;
        let types = sqlx::query!(
            r#"SELECT t.id, t.name,
                      (SELECT count(*) FROM rh_topics e
                        WHERE e.parent_id = t.id AND e.kind = 'entity') AS "entities!"
                 FROM rh_topics t
                WHERE t.kind = 'type'
                ORDER BY t.name"#
        )
        .fetch_all(&self.pool)
        .await
        .map_err(failed("entity upkeep"))?
        .into_iter()
        .map(|row| {
            Ok(TypeTally {
                topic: TopicId::from_uuid(row.id),
                name: TopicName::new(&row.name)?,
                entities: count(row.entities)?,
            })
        })
        .collect::<Result<Vec<_>, TidyError>>()?;
        Ok(WikiTally {
            topic_pages: count(pages.topic_pages)?,
            types,
            loose_pages: count(pages.loose)?,
        })
    }

    async fn tasks(&self) -> Result<Vec<TaskActivity>, TidyError> {
        let mut tasks: BTreeMap<String, TaskActivity> =
            sqlx::query!("SELECT task, finished_at FROM rh_worker_runs")
                .fetch_all(&self.pool)
                .await
                .map_err(failed("worker runs"))?
                .into_iter()
                .map(|row| {
                    (
                        row.task.clone(),
                        TaskActivity {
                            task: row.task,
                            last_finished: Some(row.finished_at),
                            failing: 0,
                            queued: 0,
                        },
                    )
                })
                .collect();
        let queued = GraphileQueue::schema_present(&self.pool)
            .await
            .map_err(|error| TidyError::Unavailable(Box::new(error)))?;
        if queued {
            let failing: Vec<FailingRow> = sqlx::query_as(
                "SELECT task, sum(failing)::bigint AS failing FROM (
                     SELECT t.identifier AS task, count(*) AS failing
                       FROM graphile_worker._private_jobs j
                       JOIN graphile_worker._private_tasks t ON t.id = j.task_id
                      WHERE j.last_error IS NOT NULL
                      GROUP BY t.identifier
                     UNION ALL
                     SELECT task, count(*) FROM rh_flagged_jobs WHERE dismissed_at IS NULL GROUP BY task
                 ) failing GROUP BY task",
            )
            .fetch_all(&self.pool)
            .await
            .map_err(failed("failing jobs"))?;
            for row in failing {
                let activity = tasks.entry(row.task.clone()).or_insert(TaskActivity {
                    task: row.task,
                    last_finished: None,
                    failing: 0,
                    queued: 0,
                });
                activity.failing = count(row.failing)?;
            }
            let waiting: Vec<FailingRow> = sqlx::query_as(
                "SELECT t.identifier AS task, count(*)::bigint AS failing
                   FROM graphile_worker._private_jobs j
                   JOIN graphile_worker._private_tasks t ON t.id = j.task_id
                  WHERE j.locked_at IS NULL AND j.attempts < j.max_attempts
                  GROUP BY t.identifier",
            )
            .fetch_all(&self.pool)
            .await
            .map_err(failed("queued jobs"))?;
            for row in waiting {
                let activity = tasks.entry(row.task.clone()).or_insert(TaskActivity {
                    task: row.task,
                    last_finished: None,
                    failing: 0,
                    queued: 0,
                });
                activity.queued = count(row.failing)?;
            }
        }
        Ok(tasks.into_values().collect())
    }
}
