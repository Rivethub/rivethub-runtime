use chrono::DateTime;
use chrono::Utc;
use kernel::wiki::entity_match::host_party_mismatch;
use kernel::wiki::entity_pass::ENTITY_LOCK_CLASS;
use kernel::wiki::entity_pass::EntityMerge;
use kernel::wiki::entity_pass::attribute_key;
use sqlx::Postgres;
use sqlx::Transaction;
use uuid::Uuid;

use super::page_file::PageFile;
use super::page_file::absorbed_page;

pub(super) async fn merge_pair(
    tx: &mut Transaction<'_, Postgres>,
    left: Uuid,
    right: Uuid,
    now: DateTime<Utc>,
) -> Result<(Option<EntityMerge>, Option<PageFile>), sqlx::Error> {
    lock_pair(tx, left, right).await?;
    let found_left = load_side(tx, left).await?;
    let found_right = load_side(tx, right).await?;
    match (found_left, found_right) {
        (Some(left_side), Some(right_side)) => merge_both(tx, left_side, right_side, now).await,
        (Some(found), None) => Ok((already(found, right), None)),
        (None, Some(found)) => Ok((already(found, left), None)),
        (None, None) => Ok((None, None)),
    }
}

async fn merge_both(
    tx: &mut Transaction<'_, Postgres>,
    left: TopicSide,
    right: TopicSide,
    now: DateTime<Utc>,
) -> Result<(Option<EntityMerge>, Option<PageFile>), sqlx::Error> {
    if left.kind != "entity" || right.kind != "entity" {
        return Ok((None, None));
    }
    if host_party_mismatch(&left.type_name, &right.type_name) {
        return Ok((None, None));
    }
    let (survivor, absorbed) = older(left, right);
    move_rows(tx, survivor.id, absorbed.id).await?;
    add_name_alias(tx, survivor.id, &absorbed.name).await?;
    let removed = redirect_wiki(tx, &survivor, &absorbed, now).await?;
    repoint(tx, survivor.id, absorbed.id, absorbed.parent_id).await?;
    retire(tx, &survivor, &absorbed, now).await?;
    Ok((
        Some(EntityMerge {
            survivor: survivor.id,
            absorbed: absorbed.id,
            survivor_name: survivor.name,
            absorbed_name: absorbed.name,
            performed: true,
        }),
        removed,
    ))
}

fn already(found: TopicSide, missing: Uuid) -> Option<EntityMerge> {
    (found.kind == "entity").then(|| EntityMerge {
        survivor: found.id,
        absorbed: missing,
        survivor_name: found.name,
        absorbed_name: String::new(),
        performed: false,
    })
}

fn older(left: TopicSide, right: TopicSide) -> (TopicSide, TopicSide) {
    if (left.created_at, left.id) <= (right.created_at, right.id) {
        (left, right)
    } else {
        (right, left)
    }
}

async fn lock_pair(
    tx: &mut Transaction<'_, Postgres>,
    left: Uuid,
    right: Uuid,
) -> Result<(), sqlx::Error> {
    let (first, second) = if left <= right {
        (left, right)
    } else {
        (right, left)
    };
    lock_one(tx, first).await?;
    if second != first {
        lock_one(tx, second).await?;
    }
    Ok(())
}

async fn lock_one(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2))")
        .bind(ENTITY_LOCK_CLASS)
        .bind(format!("id:{id}"))
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn load_side(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<TopicSide>, sqlx::Error> {
    sqlx::query_as::<_, TopicSide>(LOAD_SIDE)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
}

async fn move_rows(
    tx: &mut Transaction<'_, Postgres>,
    survivor: Uuid,
    absorbed: Uuid,
) -> Result<(), sqlx::Error> {
    move_aliases(tx, survivor, absorbed).await?;
    move_facts(tx, survivor, absorbed).await?;
    move_history(tx, survivor, absorbed).await?;
    move_types(tx, survivor, absorbed).await?;
    move_applied(tx, survivor, absorbed).await?;
    move_embedding(tx, survivor, absorbed).await?;
    Ok(())
}

async fn move_aliases(
    tx: &mut Transaction<'_, Postgres>,
    survivor: Uuid,
    absorbed: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(DROP_ALIAS_CONFLICTS)
        .bind(absorbed)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE rh_topic_aliases SET topic_id = $1 WHERE topic_id = $2")
        .bind(survivor)
        .bind(absorbed)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn add_name_alias(
    tx: &mut Transaction<'_, Postgres>,
    survivor: Uuid,
    name: &str,
) -> Result<(), sqlx::Error> {
    let trimmed = name.trim();
    let Some(key) = attribute_key(trimmed) else {
        return Ok(());
    };
    if key.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO rh_topic_aliases (topic_id, alias, alias_key, kind)
         VALUES ($1, $2, $3, 'entity')
         ON CONFLICT (kind, alias_key) DO NOTHING",
    )
    .bind(survivor)
    .bind(trimmed)
    .bind(key)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn move_facts(
    tx: &mut Transaction<'_, Postgres>,
    survivor: Uuid,
    absorbed: Uuid,
) -> Result<(), sqlx::Error> {
    let absorbed_facts = sqlx::query_as::<_, FactHead>(
        "SELECT id, key FROM rh_entity_facts WHERE topic_id = $1 ORDER BY created_at, id",
    )
    .bind(absorbed)
    .fetch_all(&mut **tx)
    .await?;
    for fact in absorbed_facts {
        let kept: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM rh_entity_facts
              WHERE topic_id = $1 AND key = $2
              ORDER BY created_at, id
              LIMIT 1",
        )
        .bind(survivor)
        .bind(&fact.key)
        .fetch_optional(&mut **tx)
        .await?;
        if let Some(keep) = kept {
            sqlx::query(DROP_OLDER_ABSORBED_CLAIMS)
                .bind(keep)
                .bind(fact.id)
                .execute(&mut **tx)
                .await?;
            sqlx::query(DROP_OLDER_SURVIVOR_CLAIMS)
                .bind(keep)
                .bind(fact.id)
                .execute(&mut **tx)
                .await?;
            sqlx::query("UPDATE rh_entity_fact_claims SET fact_id = $1 WHERE fact_id = $2")
                .bind(keep)
                .bind(fact.id)
                .execute(&mut **tx)
                .await?;
            sqlx::query("DELETE FROM rh_entity_facts WHERE id = $1")
                .bind(fact.id)
                .execute(&mut **tx)
                .await?;
        } else {
            sqlx::query("UPDATE rh_entity_facts SET topic_id = $1 WHERE id = $2")
                .bind(survivor)
                .bind(fact.id)
                .execute(&mut **tx)
                .await?;
        }
    }
    Ok(())
}

async fn move_history(
    tx: &mut Transaction<'_, Postgres>,
    survivor: Uuid,
    absorbed: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(DROP_HISTORY_CONFLICTS)
        .bind(survivor)
        .bind(absorbed)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE rh_entity_history SET entity_id = $1 WHERE entity_id = $2")
        .bind(survivor)
        .bind(absorbed)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn move_types(
    tx: &mut Transaction<'_, Postgres>,
    survivor: Uuid,
    absorbed: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(DROP_TYPE_CONFLICTS)
        .bind(survivor)
        .bind(absorbed)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE rh_entity_type_seen SET entity_id = $1 WHERE entity_id = $2")
        .bind(survivor)
        .bind(absorbed)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn move_applied(
    tx: &mut Transaction<'_, Postgres>,
    survivor: Uuid,
    absorbed: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(KEEP_LATER_MARK)
        .bind(survivor)
        .bind(absorbed)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM rh_entity_applied WHERE entity_id = $1")
        .bind(absorbed)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn move_embedding(
    tx: &mut Transaction<'_, Postgres>,
    survivor: Uuid,
    absorbed: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(DROP_OLDER_EMBEDDING)
        .bind(survivor)
        .bind(absorbed)
        .execute(&mut **tx)
        .await?;
    sqlx::query(DROP_LOSING_EMBEDDING)
        .bind(survivor)
        .bind(absorbed)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE rh_entity_embeddings SET entity_id = $1 WHERE entity_id = $2")
        .bind(survivor)
        .bind(absorbed)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn redirect_wiki(
    tx: &mut Transaction<'_, Postgres>,
    survivor: &TopicSide,
    absorbed: &TopicSide,
    now: DateTime<Utc>,
) -> Result<Option<PageFile>, sqlx::Error> {
    super::bind::bind_page(tx, survivor.id, &survivor.slug).await?;
    if survivor.slug == absorbed.slug {
        return Ok(None);
    }
    let removed = absorbed_page(tx, absorbed.id, &absorbed.slug).await?;
    ensure_page(tx, survivor).await?;
    copy_sources(tx, &absorbed.slug, &survivor.slug).await?;
    sqlx::query("UPDATE rh_wiki_redirects SET to_slug = $2 WHERE to_slug = $1 AND from_slug <> $2")
        .bind(&absorbed.slug)
        .bind(&survivor.slug)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM rh_wiki_topics WHERE slug = $1")
        .bind(&absorbed.slug)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "INSERT INTO rh_wiki_redirects (from_slug, to_slug, created_at) VALUES ($1, $2, $3)
         ON CONFLICT (from_slug) DO UPDATE SET to_slug = EXCLUDED.to_slug, created_at = EXCLUDED.created_at",
    )
    .bind(&absorbed.slug)
    .bind(&survivor.slug)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(removed)
}

async fn ensure_page(
    tx: &mut Transaction<'_, Postgres>,
    survivor: &TopicSide,
) -> Result<(), sqlx::Error> {
    let owner = if survivor.owner_user_id.is_empty() {
        "owner"
    } else {
        survivor.owner_user_id.as_str()
    };
    sqlx::query(
        "INSERT INTO rh_wiki_topics (slug, title, topic_id, owner_user_id)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (slug) DO NOTHING",
    )
    .bind(&survivor.slug)
    .bind(&survivor.name)
    .bind(survivor.id)
    .bind(owner)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn copy_sources(
    tx: &mut Transaction<'_, Postgres>,
    from_slug: &str,
    into_slug: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(COPY_CITATIONS)
        .bind(from_slug)
        .bind(into_slug)
        .execute(&mut **tx)
        .await?;
    sqlx::query(COPY_PROVENANCE)
        .bind(from_slug)
        .bind(into_slug)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn repoint(
    tx: &mut Transaction<'_, Postgres>,
    survivor: Uuid,
    absorbed: Uuid,
    absorbed_parent: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    let parent = absorbed_parent.filter(|id| *id != survivor && *id != absorbed);
    sqlx::query("UPDATE rh_topics SET parent_id = $3 WHERE id = $1 AND parent_id = $2")
        .bind(survivor)
        .bind(absorbed)
        .bind(parent)
        .execute(&mut **tx)
        .await?;
    let columns = reference_columns(tx).await?;
    for column in columns {
        if moved_already(&column.table_name) {
            continue;
        }
        let statement = update_sql(&column)?;
        sqlx::query(sqlx::AssertSqlSafe(statement))
            .bind(survivor)
            .bind(absorbed)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

fn moved_already(table: &str) -> bool {
    matches!(
        table,
        "rh_entity_facts"
            | "rh_entity_history"
            | "rh_entity_embeddings"
            | "rh_entity_applied"
            | "rh_entity_type_seen"
            | "rh_topic_aliases"
            | "rh_wiki_topics"
    )
}

fn update_sql(column: &RefColumn) -> Result<String, sqlx::Error> {
    let table = quote(&column.table_name)?;
    let field = quote(&column.column_name)?;
    if column.table_name == "rh_topics" && column.column_name == "parent_id" {
        Ok(format!(
            "UPDATE {table} SET {field} = $1 WHERE {field} = $2 AND id <> $1 AND id <> $2"
        ))
    } else {
        Ok(format!(
            "UPDATE {table} SET {field} = $1 WHERE {field} = $2"
        ))
    }
}

fn quote(name: &str) -> Result<String, sqlx::Error> {
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return Err(sqlx::Error::InvalidArgument(format!(
            "unexpected identifier {name}"
        )));
    }
    Ok(format!("\"{name}\""))
}

async fn reference_columns(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<Vec<RefColumn>, sqlx::Error> {
    let mut columns = sqlx::query_as::<_, RefColumn>(FOREIGN_KEYS)
        .fetch_all(&mut **tx)
        .await?;
    let named = sqlx::query_as::<_, RefColumn>(NAMED_IDS)
        .fetch_all(&mut **tx)
        .await?;
    columns.extend(named);
    columns.sort_by(|left, right| {
        (&left.table_name, &left.column_name).cmp(&(&right.table_name, &right.column_name))
    });
    columns.dedup_by(|left, right| {
        left.table_name == right.table_name && left.column_name == right.column_name
    });
    Ok(columns)
}

async fn retire(
    tx: &mut Transaction<'_, Postgres>,
    survivor: &TopicSide,
    absorbed: &TopicSide,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO rh_topic_tombstones (kind, slug, deleted_at) VALUES ('entity', $1, $2)
         ON CONFLICT (kind, slug) DO UPDATE SET deleted_at = EXCLUDED.deleted_at",
    )
    .bind(&absorbed.slug)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM rh_topics WHERE id = $1 AND kind = 'entity'")
        .bind(absorbed.id)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "INSERT INTO rh_topic_redirects (slug, topic_id, created_at) VALUES ($1, $2, $3)
         ON CONFLICT (slug) DO UPDATE SET topic_id = EXCLUDED.topic_id, created_at = EXCLUDED.created_at",
    )
    .bind(&absorbed.slug)
    .bind(survivor.id)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE rh_topics SET updated_at = $2 WHERE id = $1")
        .bind(survivor.id)
        .bind(now)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct TopicSide {
    id: Uuid,
    name: String,
    slug: String,
    created_at: DateTime<Utc>,
    kind: String,
    owner_user_id: String,
    type_name: String,
    parent_id: Option<Uuid>,
}

#[derive(Debug, sqlx::FromRow, PartialEq, Eq)]
struct RefColumn {
    table_name: String,
    column_name: String,
}

const LOAD_SIDE: &str = "\
SELECT t.id, t.name, t.slug, t.created_at, t.kind,
       coalesce(t.owner_user_id, '') AS owner_user_id,
       coalesce(p.name, '') AS type_name,
       t.parent_id
  FROM rh_topics t
  LEFT JOIN rh_topics p ON p.id = t.parent_id
 WHERE t.id = $1
 FOR UPDATE OF t";

const DROP_ALIAS_CONFLICTS: &str = "\
DELETE FROM rh_topic_aliases AS drop_row
 USING rh_topic_aliases AS keep_row
 WHERE drop_row.topic_id = $1
   AND keep_row.topic_id <> drop_row.topic_id
   AND keep_row.kind = drop_row.kind
   AND keep_row.alias_key = drop_row.alias_key";

#[derive(Debug, sqlx::FromRow)]
struct FactHead {
    id: Uuid,
    key: String,
}

const DROP_OLDER_ABSORBED_CLAIMS: &str = "\
DELETE FROM rh_entity_fact_claims AS drop_row
 USING rh_entity_fact_claims AS keep_row
 WHERE drop_row.fact_id = $2
   AND keep_row.fact_id = $1
   AND drop_row.summary_id IS NOT NULL
   AND drop_row.summary_id = keep_row.summary_id
   AND (drop_row.at, drop_row.recorded_at, drop_row.id)
       <= (keep_row.at, keep_row.recorded_at, keep_row.id)";

const DROP_OLDER_SURVIVOR_CLAIMS: &str = "\
DELETE FROM rh_entity_fact_claims AS keep_row
 USING rh_entity_fact_claims AS drop_row
 WHERE keep_row.fact_id = $1
   AND drop_row.fact_id = $2
   AND drop_row.summary_id IS NOT NULL
   AND drop_row.summary_id = keep_row.summary_id
   AND (drop_row.at, drop_row.recorded_at, drop_row.id)
       > (keep_row.at, keep_row.recorded_at, keep_row.id)";

const DROP_HISTORY_CONFLICTS: &str = "\
DELETE FROM rh_entity_history AS drop_row
 USING rh_entity_history AS keep_row
 WHERE drop_row.entity_id = $2
   AND keep_row.entity_id = $1
   AND drop_row.summary_id = keep_row.summary_id";

const DROP_TYPE_CONFLICTS: &str = "\
DELETE FROM rh_entity_type_seen AS drop_row
 USING rh_entity_type_seen AS keep_row
 WHERE drop_row.entity_id = $2
   AND keep_row.entity_id = $1
   AND drop_row.summary_id = keep_row.summary_id
   AND drop_row.type_name = keep_row.type_name";

const KEEP_LATER_MARK: &str = "\
INSERT INTO rh_entity_applied (entity_id, applied_through, summary_id, owner_user_id)
SELECT $1, applied_through, summary_id, owner_user_id
  FROM rh_entity_applied WHERE entity_id = $2
ON CONFLICT (entity_id) DO UPDATE SET
    applied_through = EXCLUDED.applied_through,
    summary_id = EXCLUDED.summary_id,
    owner_user_id = EXCLUDED.owner_user_id
  WHERE rh_entity_applied.applied_through < EXCLUDED.applied_through
     OR (rh_entity_applied.applied_through = EXCLUDED.applied_through
         AND rh_entity_applied.summary_id < EXCLUDED.summary_id)";

const DROP_OLDER_EMBEDDING: &str = "\
DELETE FROM rh_entity_embeddings AS older
 USING rh_entity_embeddings AS newer
 WHERE older.entity_id = $1
   AND newer.entity_id = $2
   AND newer.embedded_at >= older.embedded_at";

const DROP_LOSING_EMBEDDING: &str = "\
DELETE FROM rh_entity_embeddings AS absorbed_row
 USING rh_entity_embeddings AS survivor_row
 WHERE absorbed_row.entity_id = $2
   AND survivor_row.entity_id = $1";

const COPY_CITATIONS: &str = "\
INSERT INTO rh_wiki_citations (topic_slug, summary_id, kind, note, cited_at, owner_user_id)
SELECT $2, summary_id, kind, note, cited_at, owner_user_id
  FROM rh_wiki_citations WHERE topic_slug = $1
ON CONFLICT DO NOTHING";

const COPY_PROVENANCE: &str = "\
INSERT INTO rh_wiki_provenance
    (topic_slug, source_kind, source_id, conversation_id, git_sha, created_at, owner_user_id)
SELECT $2, source_kind, source_id, conversation_id, git_sha, created_at, owner_user_id
  FROM rh_wiki_provenance WHERE topic_slug = $1
ON CONFLICT DO NOTHING";

const FOREIGN_KEYS: &str = "\
SELECT c.relname AS table_name, a.attname AS column_name
  FROM pg_constraint con
  JOIN pg_class c ON c.oid = con.conrelid
  JOIN pg_namespace n ON n.oid = c.relnamespace
  JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum = con.conkey[1]
  JOIN pg_class f ON f.oid = con.confrelid
 WHERE con.contype = 'f'
   AND n.nspname = current_schema()
   AND f.relname = 'rh_topics'
   AND cardinality(con.conkey) = 1";

const NAMED_IDS: &str = "\
SELECT c.relname AS table_name, a.attname AS column_name
  FROM pg_attribute a
  JOIN pg_class c ON c.oid = a.attrelid
  JOIN pg_namespace n ON n.oid = c.relnamespace
  JOIN pg_type t ON t.oid = a.atttypid
 WHERE n.nspname = current_schema()
   AND c.relkind = 'r'
   AND a.attnum > 0
   AND NOT a.attisdropped
   AND t.typname = 'uuid'
   AND a.attname IN ('entity_id', 'topic_id', 'from_topic', 'to_topic', 'parent_id')";
