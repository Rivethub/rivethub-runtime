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
use rusqlite::Connection;
use rusqlite::params;

use super::at;
use super::count;
use super::failed;
use super::stored;
use super::uuid;
use crate::SqliteStore;
use crate::time::stamp;

const FILING_SQL: &str = "
SELECT count(*) FILTER (WHERE f.left_out),
       count(*) FILTER (WHERE NOT f.left_out AND f.placed_by = 'categorizer'),
       count(*) FILTER (WHERE NOT f.left_out AND f.placed_by = 'you'),
       count(*) FILTER (WHERE NOT f.left_out AND f.placed_by IS NULL),
       (SELECT count(*) FROM rh_filing_corrections WHERE corrected_at >= ?4)
  FROM (SELECT (c.channel IN (SELECT value FROM json_each(?2))
                OR EXISTS (SELECT 1 FROM json_each(?1) AS prefix
                            WHERE substr(c.session_key, 1, length(prefix.value)) = prefix.value))
                 AS left_out,
               coalesce(cp.placed_by,
                        (SELECT p.placed_by FROM rh_session_placements p
                          WHERE p.session_id IN (c.session_key, ?3 || c.session_key)
                          LIMIT 1)) AS placed_by
          FROM ros_conversations c
          LEFT JOIN rh_conversation_placements cp ON cp.conversation_id = c.id) f";

fn single(conn: &Connection, sql: &str) -> Result<u64, TidyError> {
    count(
        conn.query_row(sql, [], |row| row.get::<_, i64>(0))
            .map_err(failed("wiki upkeep"))?,
    )
}

fn types(conn: &Connection) -> Result<Vec<TypeTally>, TidyError> {
    let mut statement = conn
        .prepare(
            "SELECT t.id, t.name,
                    (SELECT count(*) FROM rh_topics e WHERE e.parent_id = t.id AND e.kind = 'entity')
               FROM rh_topics t WHERE t.kind = 'type' ORDER BY t.name",
        )
        .map_err(failed("entity upkeep"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(failed("entity upkeep"))?;
    rows.map(|row| {
        let (id, name, entities) = row.map_err(failed("entity upkeep"))?;
        Ok(TypeTally {
            topic: TopicId::from_uuid(uuid(&id, "type id")?),
            name: TopicName::new(&name)?,
            entities: count(entities)?,
        })
    })
    .collect()
}

fn tasks(conn: &Connection) -> Result<Vec<TaskActivity>, TidyError> {
    let mut tasks: BTreeMap<String, TaskActivity> = BTreeMap::new();
    let mut finished = conn
        .prepare("SELECT task, finished_at FROM rh_worker_runs")
        .map_err(failed("worker runs"))?;
    let rows = finished
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(failed("worker runs"))?;
    for row in rows {
        let (task, finished_at) = row.map_err(failed("worker runs"))?;
        tasks.insert(
            task.clone(),
            TaskActivity {
                task,
                last_finished: Some(at(&finished_at, "finished_at")?),
                failing: 0,
                queued: 0,
            },
        );
    }
    let mut failing = conn
        .prepare("SELECT task, count(*) FROM ros_jobs WHERE last_error IS NOT NULL GROUP BY task")
        .map_err(failed("failing jobs"))?;
    let rows = failing
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(failed("failing jobs"))?;
    for row in rows {
        let (task, failed_jobs) = row.map_err(failed("failing jobs"))?;
        let activity = tasks.entry(task.clone()).or_insert(TaskActivity {
            task,
            last_finished: None,
            failing: 0,
            queued: 0,
        });
        activity.failing = count(failed_jobs)?;
    }
    let mut waiting = conn
        .prepare("SELECT task, count(*) FROM ros_jobs WHERE state = 'queued' GROUP BY task")
        .map_err(failed("queued jobs"))?;
    let rows = waiting
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(failed("queued jobs"))?;
    for row in rows {
        let (task, queued_jobs) = row.map_err(failed("queued jobs"))?;
        let activity = tasks.entry(task.clone()).or_insert(TaskActivity {
            task,
            last_finished: None,
            failing: 0,
            queued: 0,
        });
        activity.queued = count(queued_jobs)?;
    }
    Ok(tasks.into_values().collect())
}

#[async_trait]
impl UpkeepStore for SqliteStore {
    async fn filing(&self, since: DateTime<Utc>) -> Result<FilingTally, TidyError> {
        let left_out = LeftOut::rules();
        let prefixes = serde_json::to_string(&left_out.key_prefixes).map_err(stored("prefixes"))?;
        let channels = serde_json::to_string(&left_out.channels).map_err(stored("channels"))?;
        self.tidy("filing upkeep", move |conn| {
            let row = conn
                .query_row(
                    FILING_SQL,
                    params![prefixes, channels, bare_session_prefix(), stamp(since)],
                    |row| {
                        Ok([
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                        ])
                    },
                )
                .map_err(failed("filing upkeep"))?;
            let [left_out, categorizer, you, uncategorized, corrections] = row;
            Ok(FilingTally {
                categorizer: count(categorizer)?,
                you: count(you)?,
                uncategorized: count(uncategorized)?,
                left_out: count(left_out)?,
                corrections: count(corrections)?,
            })
        })
        .await
    }

    async fn wiki(&self) -> Result<WikiTally, TidyError> {
        self.tidy("wiki upkeep", |conn| {
            Ok(WikiTally {
                topic_pages: single(
                    conn,
                    "SELECT count(*) FROM rh_topic_pages p JOIN rh_topics t ON t.id = p.topic_id
                      WHERE t.kind = 'work'",
                )?,
                types: types(conn)?,
                loose_pages: single(
                    conn,
                    "SELECT count(*) FROM ros_wiki_topics w
                      WHERE NOT EXISTS (SELECT 1 FROM rh_topic_pages p WHERE p.page_slug = w.slug)",
                )?,
            })
        })
        .await
    }

    async fn tasks(&self) -> Result<Vec<TaskActivity>, TidyError> {
        self.tidy("worker upkeep", tasks).await
    }
}
