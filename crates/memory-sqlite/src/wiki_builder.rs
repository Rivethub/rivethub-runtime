use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::owner::Owner;
use kernel::topics::Slug;
use kernel::wiki::builder::BACKFILL_PRIORITY;
use kernel::wiki::builder::BackfillBudget;
use kernel::wiki::builder::DeferredWiki;
use kernel::wiki::builder::ExtractWiki;
use kernel::wiki::builder::HeldEntity;
use kernel::wiki::builder::WIKI_PIPELINE_VERSION;
use kernel::wiki::builder::WikiBuilderStore;
use kernel::wiki::builder::WikiRouting;
use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::builder::WikiSummary;
use kernel::wiki::builder::WikiWork;
use kernel::wiki::page::WikiPatch;
use kernel::wiki::page::WikiSlug;
use rusqlite::Connection;
use rusqlite::OptionalExtension as _;
use rusqlite::Transaction;
use rusqlite::params;
use uuid::Uuid;

use crate::SqliteStore;
use crate::wiki_held;

#[async_trait]
impl WikiBuilderStore for SqliteStore {
    async fn wiki_work(&self, job: &ExtractWiki) -> Result<WikiWork, WikiStoreError> {
        let job = job.clone();
        blocking(self, move |conn| {
            let tx = read_tx(conn)?;
            let work = work(&tx, &job)?;
            tx.commit().map_err(query)?;
            Ok(work)
        })
        .await
    }

    async fn wiki_backfill(
        &self,
        budget: BackfillBudget,
    ) -> Result<Vec<ExtractWiki>, WikiStoreError> {
        blocking(self, move |conn| backfill(conn, budget)).await
    }

    async fn apply_wiki(
        &self,
        job: &ExtractWiki,
        patches: Vec<WikiPatch>,
        routing: &WikiRouting,
        now: DateTime<Utc>,
    ) -> Result<Vec<String>, WikiStoreError> {
        let job = job.clone();
        let routing = routing.clone();
        blocking(self, move |conn| {
            apply_all(conn, &job, patches, &routing, now)
        })
        .await
    }

    async fn fail_wiki(
        &self,
        summary_id: Uuid,
        error: &str,
        now: DateTime<Utc>,
    ) -> Result<(), WikiStoreError> {
        let error = error.to_owned();
        blocking(self, move |conn| fail(conn, summary_id, &error, now)).await
    }

    async fn hold_entity(&self, held: HeldEntity) -> Result<Vec<HeldEntity>, WikiStoreError> {
        blocking(self, move |conn| wiki_held::hold(conn, &held)).await
    }

    async fn held_entities(&self, key: &Slug) -> Result<Vec<HeldEntity>, WikiStoreError> {
        let key = key.clone();
        blocking(self, move |conn| wiki_held::candidates(conn, &key)).await
    }

    async fn release_entity(&self, held: &[HeldEntity]) -> Result<(), WikiStoreError> {
        let held = held.to_vec();
        blocking(self, move |conn| wiki_held::release(conn, &held)).await
    }

    async fn defer_wiki(&self, deferred: DeferredWiki) -> Result<(), WikiStoreError> {
        blocking(self, move |conn| wiki_held::defer(conn, &deferred)).await
    }

    async fn unsettled_notes(&self, limit: u32) -> Result<Vec<DeferredWiki>, WikiStoreError> {
        blocking(self, move |conn| wiki_held::unsettled(conn, limit)).await
    }

    async fn cite_wiki(
        &self,
        summary_id: Uuid,
        slug: &WikiSlug,
        now: DateTime<Utc>,
    ) -> Result<(), WikiStoreError> {
        let slug = slug.clone();
        blocking(self, move |conn| {
            wiki_held::cite(conn, summary_id, &slug, now)
        })
        .await
    }

    async fn uncite_wiki(&self, summary_id: Uuid, slug: &WikiSlug) -> Result<(), WikiStoreError> {
        let slug = slug.clone();
        blocking(self, move |conn| wiki_held::uncite(conn, summary_id, &slug)).await
    }
}

pub(crate) async fn blocking<T, F>(store: &SqliteStore, body: F) -> Result<T, WikiStoreError>
where
    T: Send + 'static,
    F: FnOnce(&mut Connection) -> Result<T, WikiStoreError> + Send + 'static,
{
    match store.with_conn(move |conn| Ok(body(conn))).await {
        Ok(result) => result,
        Err(error) => Err(WikiStoreError::Query(Box::new(error))),
    }
}

fn work(conn: &Connection, job: &ExtractWiki) -> Result<WikiWork, WikiStoreError> {
    let summary = load_summary(conn, job)?;
    if let Some(topics) = completed(conn, job.summary_id)? {
        return Ok(WikiWork::Complete { topics });
    }
    Ok(WikiWork::Pending(summary))
}

fn apply_all(
    conn: &mut Connection,
    job: &ExtractWiki,
    mut patches: Vec<WikiPatch>,
    routing: &WikiRouting,
    now: DateTime<Utc>,
) -> Result<Vec<String>, WikiStoreError> {
    reject_empty(&patches)?;
    let tx = write_tx(conn)?;
    let summary = load_summary(&tx, job)?;
    if let Some(topics) = completed(&tx, job.summary_id)? {
        tx.commit().map_err(query)?;
        return Ok(topics);
    }
    patches.sort_by(|left, right| left.slug.as_ref().cmp(right.slug.as_ref()));
    let mut touched = Vec::new();
    for mut patch in patches {
        let page = routing.topic_page.as_ref();
        if let Some(merged) = routing.merged(patch.slug.as_ref()) {
            crate::wiki_topics::rebase_merged(&tx, &mut patch, &merged.base)?;
        }
        if let Some(slug) = crate::wiki_topics::apply(&tx, &summary, &patch, page, now)? {
            touched.push(slug);
        }
    }
    for slug in &routing.cited {
        crate::wiki_topics::cite(&tx, slug.as_ref(), &summary, now)?;
    }
    touched.sort();
    touched.dedup();
    record_done(&tx, job.summary_id, &touched, now)?;
    tx.commit().map_err(query)?;
    for slug in &touched {
        tracing::info!("wiki topic stored {slug}");
    }
    Ok(touched)
}

fn reject_empty(patches: &[WikiPatch]) -> Result<(), WikiStoreError> {
    for patch in patches {
        if patch.title.trim().is_empty() || patch.summary.trim().is_empty() {
            return Err(WikiStoreError::InvalidPatch(
                "title and summary must be nonempty".to_owned(),
            ));
        }
    }
    Ok(())
}

fn backfill(conn: &Connection, budget: BackfillBudget) -> Result<Vec<ExtractWiki>, WikiStoreError> {
    let mut statement = conn
        .prepare(
            "SELECT s.id, s.conversation_id FROM ros_summaries s
             LEFT JOIN ros_conversations c ON c.id = s.conversation_id
             LEFT JOIN ros_wiki_extractions e ON e.summary_id = s.id
             WHERE s.kind = 'leaf'
               AND (c.id IS NULL OR c.session_key NOT LIKE 'heartbeat:%')
               AND (e.summary_id IS NULL OR e.status = 'failed')
               AND NOT EXISTS (SELECT 1 FROM ros_jobs j WHERE j.job_key = 'wiki-ext-' || s.id)
             ORDER BY s.created_at DESC, s.id
             LIMIT max(0, (SELECT CASE WHEN EXISTS (
                         SELECT 1 FROM ros_jobs n LEFT JOIN rh_job_priorities p ON p.job_id = n.id
                          WHERE n.state = 'queued'
                            AND ((n.task = 'extract-wiki' AND coalesce(p.priority, 0) < ?3)
                              OR (n.task = 'compact-conversation' AND coalesce(p.priority, 0) < 0)))
                       THEN ?2 ELSE ?1 END)
                     - (SELECT count(*) FROM ros_jobs q JOIN rh_job_priorities p ON p.job_id = q.id
                         WHERE q.job_key LIKE 'wiki-ext-%' AND q.state <> 'dead' AND p.priority = ?3))",
        )
        .map_err(query)?;
    let rows = statement
        .query_map(
            params![
                i64::from(budget.idle),
                i64::from(budget.busy),
                BACKFILL_PRIORITY
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .map_err(query)?;
    let mut jobs = Vec::new();
    for row in rows {
        let (id, conversation) = row.map_err(query)?;
        jobs.push(ExtractWiki {
            summary_id: parse_uuid(&id)?,
            conversation_id: optional_uuid(conversation)?,
        });
    }
    Ok(jobs)
}

fn fail(
    conn: &Connection,
    summary_id: Uuid,
    error: &str,
    now: DateTime<Utc>,
) -> Result<(), WikiStoreError> {
    conn.execute(
        "INSERT INTO ros_wiki_extractions
            (summary_id, status, pipeline_version, error, extracted_at)
         VALUES (?1, 'failed', ?2, ?3, ?4)
         ON CONFLICT(summary_id) DO UPDATE SET status = 'failed',
            pipeline_version = excluded.pipeline_version, error = excluded.error,
            extracted_at = excluded.extracted_at
         WHERE ros_wiki_extractions.status = 'failed'
            OR ros_wiki_extractions.pipeline_version < ?2",
        params![
            summary_id.to_string(),
            WIKI_PIPELINE_VERSION,
            error,
            time_stamp(now)
        ],
    )
    .map_err(query)?;
    Ok(())
}

fn record_done(
    conn: &Connection,
    summary_id: Uuid,
    touched: &[String],
    now: DateTime<Utc>,
) -> Result<(), WikiStoreError> {
    let topics =
        serde_json::to_string(touched).map_err(|source| WikiStoreError::Query(Box::new(source)))?;
    conn.execute(
        "INSERT INTO ros_wiki_extractions
            (summary_id, status, pipeline_version, topics_touched, extracted_at)
         VALUES (?1, 'done', ?2, ?3, ?4)
         ON CONFLICT(summary_id) DO UPDATE SET status = 'done',
            pipeline_version = excluded.pipeline_version,
            topics_touched = excluded.topics_touched, error = NULL,
            extracted_at = excluded.extracted_at",
        params![
            summary_id.to_string(),
            WIKI_PIPELINE_VERSION,
            topics,
            time_stamp(now)
        ],
    )
    .map_err(query)?;
    Ok(())
}

pub(crate) fn load_summary(
    conn: &Connection,
    job: &ExtractWiki,
) -> Result<WikiSummary, WikiStoreError> {
    let row = conn
        .query_row(
            "SELECT s.id, s.conversation_id, s.content, c.session_key, c.owner_user_id, s.created_at
               FROM ros_summaries s
               LEFT JOIN ros_conversations c ON c.id = s.conversation_id WHERE s.id = ?1",
            params![job.summary_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()
        .map_err(query)?
        .ok_or(WikiStoreError::Missing(job.summary_id))?;
    let id = parse_uuid(&row.0)?;
    let conversation_id = optional_uuid(row.1)?;
    if job
        .conversation_id
        .is_some_and(|expected| Some(expected) != conversation_id)
    {
        return Err(WikiStoreError::ConversationMismatch(job.summary_id));
    }
    Ok(WikiSummary {
        id,
        conversation_id,
        content: row.2,
        session_key: row.3,
        owner: row
            .4
            .map(Owner::new)
            .transpose()
            .map_err(|error| WikiStoreError::Query(Box::new(error)))?,
        created_at: Some(
            crate::time::stored(&row.5, "created_at")
                .map_err(|error| WikiStoreError::Query(Box::new(error)))?,
        ),
    })
}

fn completed(conn: &Connection, summary_id: Uuid) -> Result<Option<Vec<String>>, WikiStoreError> {
    let topics = conn
        .query_row(
            "SELECT topics_touched FROM ros_wiki_extractions
              WHERE summary_id = ?1 AND status IN ('done', 'skipped') AND pipeline_version >= ?2",
            params![summary_id.to_string(), WIKI_PIPELINE_VERSION],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(query)?;
    match topics {
        None => Ok(None),
        Some(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|source| WikiStoreError::Query(Box::new(source))),
    }
}

fn read_tx(conn: &mut Connection) -> Result<Transaction<'_>, WikiStoreError> {
    conn.transaction().map_err(query)
}

fn write_tx(conn: &mut Connection) -> Result<Transaction<'_>, WikiStoreError> {
    conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(query)
}

fn parse_uuid(text: &str) -> Result<Uuid, WikiStoreError> {
    Uuid::parse_str(text).map_err(|source| WikiStoreError::Query(Box::new(source)))
}

fn optional_uuid(value: Option<String>) -> Result<Option<Uuid>, WikiStoreError> {
    match value {
        None => Ok(None),
        Some(text) => parse_uuid(&text).map(Some),
    }
}

fn time_stamp(now: DateTime<Utc>) -> String {
    crate::time::stamp(now)
}

pub(crate) fn query(source: rusqlite::Error) -> WikiStoreError {
    WikiStoreError::Query(Box::new(source))
}
