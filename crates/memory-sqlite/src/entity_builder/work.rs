use jiff::{SignedDuration, Timestamp};
use kernel::memory::summary_reference::{SummaryConsumer, SummaryReferenceError};
use kernel::wiki::builder::{BackfillBudget, SummaryDelivery};
use kernel::wiki::entity_pass::{
    ENTITY_PIPELINE_VERSION, EntityLeaf, EntityStoreError, EntityWork, ExtractEntities,
    MatchDecision, PreparedEntity,
};
use rusqlite::{Connection, OptionalExtension as _, Row, Transaction, params};
use serde::Serialize;
use uuid::Uuid;

use super::{at, error, fence, optional_uuid, query, rows, stamp, uuid};
use crate::summary_replace::{self, SummaryInput};

const BACKFILL_PRIORITY: i16 = 50;

struct LeafRow {
    id: String,
    conversation: Option<String>,
    content: String,
    start: String,
    end: String,
    owner: Option<String>,
}

impl LeafRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            conversation: row.get(1)?,
            content: row.get(2)?,
            start: row.get(3)?,
            end: row.get(4)?,
            owner: row.get(5)?,
        })
    }

    fn into_leaf(self) -> Result<EntityLeaf, EntityStoreError> {
        let start = at(&self.start)?;
        Ok(EntityLeaf {
            id: uuid(&self.id)?,
            conversation_id: optional_uuid(self.conversation.as_deref())?,
            content: self.content,
            start,
            end: at(&self.end)?.max(start),
            owner: self.owner,
            delivery: None,
        })
    }
}

pub(super) fn load(
    conn: &Connection,
    job: &ExtractEntities,
) -> Result<EntityWork, EntityStoreError> {
    let (row, key): (LeafRow, Option<Vec<u8>>) = conn.query_row(
        "SELECT s.id, s.conversation_id, s.content,
            coalesce(s.earliest_at, s.created_at), coalesce(s.latest_at, s.earliest_at, s.created_at),
            c.owner_user_id, s.input_key
         FROM ros_summaries s LEFT JOIN ros_conversations c ON c.id = s.conversation_id
         WHERE s.id = ?1",
        [job.summary_id.to_string()], |row| Ok((LeafRow::read(row)?, row.get(6)?)),
    ).optional().map_err(query)?.ok_or(EntityStoreError::Missing(job.summary_id))?;
    let mut leaf = row.into_leaf()?;
    if job
        .conversation_id
        .is_some_and(|wanted| leaf.conversation_id != Some(wanted))
    {
        return Err(SummaryReferenceError::Stale(leaf.id).into());
    }
    if let Some(input_key) = key {
        let pending = pending(conn, leaf.id)?;
        if pending.is_empty() {
            return Ok(EntityWork::Complete {
                entities: Vec::new(),
            });
        }
        leaf.delivery = Some(SummaryDelivery { input_key, pending });
    } else if let Some(entities) = done(conn, leaf.id)? {
        return Ok(EntityWork::Complete { entities });
    }
    Ok(EntityWork::Pending(leaf))
}

fn done(conn: &Connection, id: Uuid) -> Result<Option<Vec<String>>, EntityStoreError> {
    let payload: Option<String> = conn
        .query_row(
            "SELECT entities_touched FROM rh_entity_extractions
         WHERE summary_id = ?1 AND status = 'done' AND pipeline_version >= ?2",
            params![id.to_string(), ENTITY_PIPELINE_VERSION],
            |row| row.get(0),
        )
        .optional()
        .map_err(query)?;
    payload
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(error)
}

pub(super) fn following(
    conn: &Connection,
    lead: &EntityLeaf,
    limit: u32,
) -> Result<Vec<(EntityLeaf, bool, Option<String>)>, EntityStoreError> {
    let Some(conversation) = lead.conversation_id.filter(|_| lead.delivery.is_none()) else {
        return Ok(Vec::new());
    };
    rows(conn,
        "SELECT s.id, s.conversation_id, s.content,
            coalesce(s.earliest_at, s.created_at), coalesce(s.latest_at, s.earliest_at, s.created_at),
            c.owner_user_id, coalesce(e.status = 'done' AND e.pipeline_version >= ?4, 0), j.id
         FROM ros_summaries s JOIN ros_conversations c ON c.id = s.conversation_id
         LEFT JOIN rh_entity_extractions e ON e.summary_id = s.id
         LEFT JOIN ros_jobs j ON j.job_key = 'entity-ext-' || s.id AND j.task = 'extract-entities'
         WHERE s.conversation_id = ?1 AND s.kind = 'leaf'
           AND (rh_instant(coalesce(s.earliest_at, s.created_at)), s.id) > (?2, ?3)
         ORDER BY rh_instant(coalesce(s.earliest_at, s.created_at)), s.id LIMIT ?5",
        params![conversation.to_string(), crate::time::seek_key(lead.start), lead.id.to_string(),
            ENTITY_PIPELINE_VERSION, limit],
        |row| Ok((LeafRow::read(row)?, row.get(6)?, row.get(7)?)),
    )?.into_iter().map(|(row, done, job)| Ok((row.into_leaf()?, done, job))).collect()
}

fn pending(conn: &Connection, id: Uuid) -> Result<Vec<Uuid>, EntityStoreError> {
    summary_replace::pending(conn, id, SummaryConsumer::Entity).map_err(query)
}

pub(super) fn validate_leaf(
    tx: &Transaction<'_>,
    input: &SummaryInput,
    leaf: &EntityLeaf,
) -> Result<(), EntityStoreError> {
    if input.conversation_id != leaf.conversation_id.map(|id| id.to_string())
        || input.owner != leaf.owner
        || input.content != leaf.content
        || input.input_key.as_ref() != leaf.delivery.as_ref().map(|delivery| &delivery.input_key)
    {
        return Err(SummaryReferenceError::Stale(leaf.id).into());
    }
    if let Some(delivery) = &leaf.delivery
        && pending(tx, leaf.id)? != delivery.pending
    {
        return Err(SummaryReferenceError::Stale(leaf.id).into());
    }
    Ok(())
}

pub(super) fn guard(
    tx: &Transaction<'_>,
    ids: impl IntoIterator<Item = Uuid>,
    owner: &str,
) -> Result<(), EntityStoreError> {
    for id in ids {
        let input = summary_replace::shared(tx, id).map_err(fence)?;
        if input.owner.as_deref() != Some(owner) {
            return Err(SummaryReferenceError::Stale(id).into());
        }
    }
    Ok(())
}

fn owned(input: &SummaryInput) -> &str {
    input.owner.as_deref().unwrap_or("owner")
}

pub(super) fn finish(
    tx: &Transaction<'_>,
    ids: &[Uuid],
    leaves: &[EntityLeaf],
    slugs: &[String],
    sha: Option<&str>,
    now: Timestamp,
) -> Result<(), EntityStoreError> {
    let slugs = serde_json::to_string(slugs).map_err(error)?;
    for id in ids {
        let input = summary_replace::shared(tx, *id).map_err(fence)?;
        let leaf = leaves.iter().find(|leaf| leaf.id == *id);
        if let Some(leaf) = leaf {
            validate_leaf(tx, &input, leaf)?;
        } else if input.input_key.is_some() {
            return Err(SummaryReferenceError::Stale(*id).into());
        }
        tx.execute(
            "INSERT INTO rh_entity_extractions
                (summary_id, status, pipeline_version, entities_touched, git_sha, extracted_at, owner_user_id)
             VALUES (?1, 'done', ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (summary_id) DO UPDATE SET status = 'done',
                pipeline_version = excluded.pipeline_version, entities_touched = excluded.entities_touched,
                git_sha = excluded.git_sha, error = NULL, extracted_at = excluded.extracted_at",
            params![id.to_string(), ENTITY_PIPELINE_VERSION, slugs, sha, stamp(now), owned(&input)],
        ).map_err(query)?;
        if let Some(delivery) = leaf.and_then(|leaf| leaf.delivery.as_ref()) {
            summary_replace::complete(
                tx,
                *id,
                SummaryConsumer::Entity,
                &delivery.pending,
                ENTITY_PIPELINE_VERSION,
            )
            .map_err(fence)?;
        } else {
            summary_replace::complete_legacy(
                tx,
                *id,
                SummaryConsumer::Entity,
                ENTITY_PIPELINE_VERSION,
            )
            .map_err(query)?;
        }
    }
    Ok(())
}

pub(super) fn fail(
    tx: &Transaction<'_>,
    id: Uuid,
    failure: &str,
    now: Timestamp,
) -> Result<(), EntityStoreError> {
    let input = summary_replace::shared(tx, id).map_err(fence)?;
    tx.execute(
        "INSERT INTO rh_entity_extractions
            (summary_id, status, pipeline_version, error, extracted_at, owner_user_id)
         VALUES (?1, 'failed', ?2, ?3, ?4, ?5)
         ON CONFLICT (summary_id) DO UPDATE SET status = 'failed',
            pipeline_version = excluded.pipeline_version, error = excluded.error,
            extracted_at = excluded.extracted_at
         WHERE rh_entity_extractions.status <> 'done' OR rh_entity_extractions.pipeline_version < ?2",
        params![id.to_string(), ENTITY_PIPELINE_VERSION, failure, stamp(now), owned(&input)],
    ).map_err(query)?;
    Ok(())
}

pub(super) fn backfill(
    conn: &Connection,
    budget: BackfillBudget,
    now: Timestamp,
) -> Result<Vec<ExtractEntities>, EntityStoreError> {
    jobs(conn,
        "SELECT s.id, s.conversation_id, coalesce(s.earliest_at, s.created_at)
         FROM ros_summaries s
         LEFT JOIN ros_conversations c ON c.id = s.conversation_id
         LEFT JOIN rh_entity_extractions e ON e.summary_id = s.id
         WHERE ((s.kind = 'leaf' AND (e.summary_id IS NULL OR e.status = 'failed' OR e.pipeline_version < ?1)
                 AND NOT EXISTS (SELECT 1 FROM rh_summary_conversion_items i
                                  WHERE i.conversation_id = s.conversation_id AND i.state <> 'complete'))
             OR EXISTS (SELECT 1 FROM ros_summary_deliveries d WHERE d.anchor_id = s.id AND d.consumer = 'entity'))
           AND (c.id IS NULL OR c.session_key NOT LIKE 'heartbeat:%')
           AND NOT EXISTS (SELECT 1 FROM ros_jobs j WHERE j.job_key = 'entity-ext-' || s.id)
         ORDER BY rh_instant(coalesce(s.earliest_at, s.created_at)), s.id
         LIMIT max(0, (SELECT CASE WHEN EXISTS (
            SELECT 1 FROM ros_jobs n LEFT JOIN rh_job_priorities p ON p.job_id = n.id
            WHERE n.state = 'queued' AND rh_instant(n.run_at) <= ?5
              AND ((n.task = 'extract-entities' AND coalesce(p.priority, 0) < ?4)
                OR (n.task = 'compact-conversation' AND coalesce(p.priority, 0) < 0)))
            THEN ?3 ELSE ?2 END)
          - (SELECT count(*) FROM ros_jobs q JOIN rh_job_priorities p ON p.job_id = q.id
             WHERE q.job_key LIKE 'entity-ext-%' AND p.priority = ?4 AND q.state <> 'dead'))",
        params![ENTITY_PIPELINE_VERSION, budget.idle, budget.busy, BACKFILL_PRIORITY,
            crate::time::seek_key(now)],
    )
}

pub(super) fn stale(
    conn: &Connection,
    limit: u32,
    now: Timestamp,
) -> Result<Vec<ExtractEntities>, EntityStoreError> {
    let cutoff = now
        .checked_sub(SignedDuration::from_hours(24))
        .map_err(error)?;
    jobs(
        conn,
        "SELECT s.id, s.conversation_id, coalesce(s.earliest_at, s.created_at)
         FROM ros_summaries s LEFT JOIN rh_entity_extractions e ON e.summary_id = s.id
         WHERE s.kind = 'leaf' AND rh_instant(s.created_at) > ?3
           AND (e.summary_id IS NULL OR e.status <> 'done' OR e.pipeline_version < ?1)
           AND NOT EXISTS (SELECT 1 FROM ros_jobs j WHERE j.job_key = 'entity-ext-' || s.id)
         ORDER BY rh_instant(coalesce(s.earliest_at, s.created_at)), s.id LIMIT ?2",
        params![
            ENTITY_PIPELINE_VERSION,
            limit,
            crate::time::seek_key(cutoff)
        ],
    )
}

fn jobs(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<ExtractEntities>, EntityStoreError> {
    rows(conn, sql, params, |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?
    .into_iter()
    .map(|(id, conversation, session_at)| {
        Ok(ExtractEntities {
            summary_id: uuid(&id)?,
            conversation_id: optional_uuid(conversation.as_deref())?,
            session_at: Some(at(&session_at)?),
        })
    })
    .collect()
}

pub(super) fn revive(
    tx: &Transaction<'_>,
    limit: u32,
    now: Timestamp,
) -> Result<u64, EntityStoreError> {
    let ids = rows(tx,
        "UPDATE ros_jobs SET state = 'queued', attempts = 0, run_at = ?2, locked_at = NULL, updated_at = ?2
         WHERE id IN (SELECT id FROM ros_jobs WHERE task = 'extract-entities' AND state = 'dead'
                      AND job_key IS NOT NULL AND attempts >= max_attempts
                      AND rh_instant(updated_at) < ?3
                      AND (locked_at IS NULL OR rh_instant(locked_at) < ?4)
                      ORDER BY rh_instant(run_at), rowid LIMIT ?1)
         RETURNING id", params![limit, crate::time::stamp(now), cutoff(now, 24)?, cutoff(now, 4)?], |row| row.get::<_, String>(0))?;
    for id in &ids {
        crate::job_rank::rank(tx, id, BACKFILL_PRIORITY).map_err(error)?;
    }
    u64::try_from(ids.len()).map_err(error)
}

fn cutoff(now: Timestamp, hours: i64) -> Result<String, EntityStoreError> {
    now.checked_sub(SignedDuration::from_hours(hours))
        .map(crate::time::seek_key)
        .map_err(error)
}

#[derive(Serialize)]
struct HeldFact<'a> {
    attribute: &'a str,
    value: &'a str,
}

#[derive(Serialize)]
struct HeldEntity<'a> {
    name: &'a str,
    #[serde(rename = "type")]
    type_name: &'a str,
    aliases: &'a [String],
    entity_ref: Option<Uuid>,
    facts: Vec<HeldFact<'a>>,
}

pub(super) fn hold(
    tx: &Transaction<'_>,
    summary: Uuid,
    item: &PreparedEntity,
    confidence: f32,
    reason: &str,
    now: Timestamp,
) -> Result<(), EntityStoreError> {
    let input = summary_replace::shared(tx, summary).map_err(fence)?;
    let payload = serde_json::to_string(&HeldEntity {
        name: &item.name,
        type_name: &item.type_name,
        aliases: &item.aliases,
        entity_ref: item.entity_ref,
        facts: item
            .facts
            .iter()
            .map(|fact| HeldFact {
                attribute: &fact.attribute,
                value: &fact.value,
            })
            .collect(),
    })
    .map_err(error)?;
    tx.execute(
        "INSERT INTO rh_entity_candidates
            (id, summary_id, name, type_name, confidence, reason, payload, created_at, owner_user_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![Uuid::new_v4().to_string(), summary.to_string(), item.name, item.type_name,
            confidence, reason, payload, stamp(now), owned(&input)],
    ).map_err(query)?;
    Ok(())
}

pub(super) fn decision(
    tx: &Transaction<'_>,
    summary: Uuid,
    item: &MatchDecision,
    now: Timestamp,
) -> Result<(), EntityStoreError> {
    let input = summary_replace::shared(tx, summary).map_err(fence)?;
    let candidates = serde_json::to_string(&item.candidates).map_err(error)?;
    let choice = (!item.choice.is_empty()).then_some(&item.choice);
    tx.execute(
        "INSERT INTO rh_entity_decisions
            (id, summary_id, kind, proposed_name, proposed_type, candidates, verdict, choice,
             confidence, model, entity_id, created_at, owner_user_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            Uuid::new_v4().to_string(),
            summary.to_string(),
            item.kind,
            item.proposed_name,
            item.proposed_type,
            candidates,
            item.verdict,
            choice,
            item.confidence,
            item.model,
            item.entity_id.map(|id| id.to_string()),
            stamp(now),
            owned(&input)
        ],
    )
    .map_err(query)?;
    Ok(())
}
