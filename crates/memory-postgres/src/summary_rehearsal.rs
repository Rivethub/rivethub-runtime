use std::collections::BTreeSet;

use kernel::memory::summary_conversion::{ConversionRehearsal, REHEARSAL_LIMIT};
use kernel::memory::summary_reference::replacement_target;
use sqlx::{AssertSqlSafe, PgConnection};
use uuid::Uuid;

use crate::{Error, PostgresMemory, summary_replace::publish::old_coverage};

fn error(source: sqlx::Error) -> Error {
    Error::operation("rehearse summary references", source)
}

const REFERENCES: &[(&str, &str, &str, &str)] = &[
    ("rh_summary_sources", "summary_id", "NULL", "true"),
    ("rh_summary_passages", "record_id", "NULL", "true"),
    ("rh_summary_access", "summary_id", "1", "true"),
    (
        "rh_wiki_citations",
        "summary_id",
        "r.topic_slug",
        "r.owner_user_id = $3",
    ),
    (
        "rh_wiki_provenance",
        "source_id",
        "r.topic_slug",
        "r.owner_user_id = $3 AND r.source_kind = 'summary'",
    ),
    (
        "rh_wiki_extractions",
        "summary_id",
        "1",
        "r.owner_user_id = $3",
    ),
    (
        "rh_entity_extractions",
        "summary_id",
        "1",
        "r.owner_user_id = $3",
    ),
    (
        "rh_wiki_deferred",
        "summary_id",
        "1",
        "r.owner_user_id = $3",
    ),
    (
        "rh_entity_history",
        "summary_id",
        "r.entity_id",
        "r.owner_user_id = $3",
    ),
    (
        "rh_entity_type_seen",
        "summary_id",
        "(r.entity_id, r.type_name)",
        "r.owner_user_id = $3",
    ),
    (
        "rh_entity_candidates",
        "summary_id",
        "r.id",
        "r.owner_user_id = $3",
    ),
    (
        "rh_entity_decisions",
        "summary_id",
        "r.id",
        "r.owner_user_id = $3",
    ),
    (
        "rh_entity_applied",
        "summary_id",
        "r.entity_id",
        "r.owner_user_id = $3",
    ),
    (
        "rh_entity_fact_claims",
        "summary_id",
        "r.fact_id",
        "r.owner_user_id = $3",
    ),
    (
        "rh_tags",
        "entity_id",
        "(r.key, r.value)",
        "r.entity_type = 'summary'",
    ),
    ("rh_summary_deliveries", "anchor_id", "NULL", "true"),
    ("rh_summary_runs", "expected_root", "NULL", "true"),
    (
        "rh_wiki_entity_candidates",
        "summary_id",
        "(r.key, CASE WHEN r.source = r.summary_id::text THEN ':root'
                      WHEN r.source = 'summary:' || r.summary_id THEN 'summary::root'
                      ELSE r.source END)",
        "r.owner_user_id = $3",
    ),
];

impl PostgresMemory {
    pub async fn rehearse_conversion(&self, ids: &[Uuid]) -> Result<ConversionRehearsal, Error> {
        if ids.is_empty() || ids.len() > REHEARSAL_LIMIT {
            return Err(Error::protocol(
                "the rehearsal requires 1 to 20 conversation IDs",
            ));
        }
        let mut tx = self.pool.begin().await.map_err(error)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(error)?;
        let mut result = ConversionRehearsal::default();
        for id in ids.iter().collect::<BTreeSet<_>>() {
            rehearse(&mut tx, *id, &mut result).await?;
        }
        tx.commit().await.map_err(error)?;
        Ok(result)
    }
}

async fn rehearse(
    conn: &mut PgConnection,
    id: Uuid,
    result: &mut ConversionRehearsal,
) -> Result<(), Error> {
    let owner: Option<String> =
        sqlx::query_scalar("SELECT owner_user_id FROM rh_conversations WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(error)?;
    let Some(owner) = owner else { return Ok(()) };
    let old: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM rh_summaries WHERE conversation_id = $1 AND kind IN ('leaf','branch','root')",
    ).bind(id).fetch_all(&mut *conn).await.map_err(error)?;
    if old.is_empty() {
        return Ok(());
    }
    let coverage = old_coverage(conn, id, &owner).await?;
    let renderable: Vec<_> = sqlx::query_scalar(
        "SELECT id FROM rh_messages WHERE conversation_id = $1
           AND (content <> '' OR coalesce(tool_name <> '', false))",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(error)?;
    let renderable: BTreeSet<_> = renderable.into_iter().collect();
    let mapped: Vec<_> = old
        .iter()
        .copied()
        .filter(|id| {
            coverage.get(id).is_some_and(|members| {
                replacement_target(members, [(Uuid::nil(), &renderable)]).is_some()
            })
        })
        .collect();
    result.sampled_conversations += 1;
    for &(table, link, grouping, condition) in REFERENCES {
        let filter = match table {
            "rh_wiki_provenance" => "r.source_kind = 'summary'",
            "rh_tags" => "r.entity_type = 'summary'",
            _ => "true",
        };
        let (total, remapped): (i64, i64) = sqlx::query_as(AssertSqlSafe(format!(
            "SELECT count(*), count(DISTINCT {grouping}) FILTER (
                 WHERE r.{link} = ANY($2) AND $3::text IS NOT NULL AND ({condition}))
             FROM {table} r WHERE r.{link} = ANY($1) AND {filter}"
        )))
        .bind(&old)
        .bind(&mapped)
        .bind(&owner)
        .fetch_one(&mut *conn)
        .await
        .map_err(error)?;
        result.add(table, total, remapped);
    }
    let queue: bool =
        sqlx::query_scalar("SELECT to_regclass('graphile_worker._private_jobs') IS NOT NULL")
            .fetch_one(&mut *conn)
            .await
            .map_err(error)?;
    let old_text: Vec<_> = old.iter().map(Uuid::to_string).collect();
    if queue {
        let count = sqlx::query_scalar(
            "SELECT count(*) FROM graphile_worker._private_jobs WHERE locked_at IS NULL AND (
               payload->>'summaryId' = ANY($1) OR (
                 payload->>'targetTable' = 'ros_summaries' AND payload->>'targetId' = ANY($1)))",
        )
        .bind(&old_text)
        .fetch_one(&mut *conn)
        .await
        .map_err(error)?;
        result.add("graphile_worker._private_jobs", count, 0);
    }
    let count = sqlx::query_scalar(
        "SELECT count(*) FROM rh_flagged_jobs WHERE payload->>'summaryId' = ANY($1) OR (
           payload->>'targetTable' = 'ros_summaries' AND payload->>'targetId' = ANY($1))",
    )
    .bind(&old_text)
    .fetch_one(&mut *conn)
    .await
    .map_err(error)?;
    result.add("rh_flagged_jobs", count, 0);
    let count = sqlx::query_scalar(
        "SELECT count(*) FROM rh_job_failures f JOIN unnest($1::text[]) old(id)
           ON f.job_key IN ('wiki-ext-' || old.id, 'entity-ext-' || old.id, 'embed-ros_summaries-' || old.id)",
    ).bind(&old_text).fetch_one(conn).await.map_err(error)?;
    result.add("rh_job_failures", count, 0);
    Ok(())
}
