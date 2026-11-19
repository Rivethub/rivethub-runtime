use sqlx::PgConnection;
use uuid::Uuid;

use super::MergeReport;
use super::MergeRequest;
use super::stage::staged;
use super::table::Table;
use crate::error::Error;

const SETUP: &str = "
CREATE TEMP TABLE merge_new_conversations (id uuid PRIMARY KEY) ON COMMIT DROP;
CREATE TEMP TABLE merge_moves (conversation uuid PRIMARY KEY, owner text NOT NULL) ON COMMIT DROP;
CREATE TEMP TABLE merge_drop (id uuid PRIMARY KEY) ON COMMIT DROP;
CREATE TEMP TABLE merge_topic_map (source_id uuid PRIMARY KEY, target_id uuid NOT NULL) ON COMMIT DROP;
CREATE TEMP TABLE merge_slug_map (old text PRIMARY KEY, new text NOT NULL) ON COMMIT DROP;
SELECT t.id FROM rh_conversations t
  JOIN merge_rh_conversations s USING (session_key, agent)
 WHERE t.id <> s.id ORDER BY t.id FOR UPDATE OF t;
CREATE TEMP TABLE merge_dupes ON COMMIT DROP AS
  SELECT pairs.*, source_messages > target_messages AND NOT needs_repair AND NOT has_visible_tree AS source_wins FROM (
  SELECT s.id AS source_id, t.id AS target_id, s.session_key, s.agent, t.owner_user_id AS target_owner,
         EXISTS (SELECT 1 FROM rh_summary_state state WHERE state.conversation_id = t.id
                 AND NOT state.historical_only) AS target_live,
         (SELECT count(*) FROM merge_rh_messages m WHERE m.conversation_id = s.id) AS source_messages,
         (SELECT count(*) FROM rh_messages m WHERE m.conversation_id = t.id) AS target_messages,
         (EXISTS (SELECT 1 FROM merge_rh_summaries a WHERE a.conversation_id = s.id AND a.index_needs_repair)
          AND EXISTS (SELECT 1 FROM rh_summaries b WHERE b.conversation_id = t.id)) AS needs_repair,
         EXISTS (SELECT 1 FROM rh_summaries existing WHERE existing.conversation_id = t.id
                 AND existing.kind = 'session') AS has_visible_tree
    FROM merge_rh_conversations s
    JOIN rh_conversations t ON t.session_key = s.session_key AND t.agent = s.agent AND t.id <> s.id
  ) pairs;
";

const DROP_STAGED: &str = "
DELETE FROM merge_rh_summary_deliveries WHERE conversation_id IN (SELECT id FROM merge_drop);
DELETE FROM merge_rh_summary_passages WHERE record_id IN (
    SELECT s.id FROM merge_rh_summaries s JOIN merge_drop d ON d.id = s.conversation_id);
DELETE FROM merge_rh_tags WHERE entity_id IN (
  SELECT id FROM merge_drop
  UNION SELECT m.id FROM merge_rh_messages m JOIN merge_drop d ON d.id = m.conversation_id
  UNION SELECT s.id FROM merge_rh_summaries s JOIN merge_drop d ON d.id = s.conversation_id);
DELETE FROM merge_rh_wiki_extractions WHERE summary_id IN (SELECT s.id FROM merge_rh_summaries s JOIN merge_drop d ON d.id = s.conversation_id);
DELETE FROM merge_rh_wiki_citations WHERE summary_id IN (SELECT s.id FROM merge_rh_summaries s JOIN merge_drop d ON d.id = s.conversation_id);
DELETE FROM merge_rh_wiki_entity_candidates WHERE summary_id IN (SELECT s.id FROM merge_rh_summaries s JOIN merge_drop d ON d.id = s.conversation_id);
DELETE FROM merge_rh_wiki_deferred WHERE summary_id IN (SELECT s.id FROM merge_rh_summaries s JOIN merge_drop d ON d.id = s.conversation_id);
DELETE FROM merge_rh_wiki_provenance WHERE source_id IN (SELECT s.id FROM merge_rh_summaries s JOIN merge_drop d ON d.id = s.conversation_id);
DELETE FROM merge_rh_summary_sources WHERE summary_id IN (SELECT s.id FROM merge_rh_summaries s JOIN merge_drop d ON d.id = s.conversation_id);
DELETE FROM merge_rh_summaries WHERE conversation_id IN (SELECT id FROM merge_drop);
DELETE FROM merge_rh_message_chunks WHERE message_id IN (SELECT m.id FROM merge_rh_messages m JOIN merge_drop d ON d.id = m.conversation_id);
DELETE FROM merge_rh_messages WHERE conversation_id IN (SELECT id FROM merge_drop);
DELETE FROM merge_rh_conversation_placements WHERE conversation_id IN (SELECT id FROM merge_drop);
DELETE FROM merge_rh_filing_corrections WHERE conversation_id IN (SELECT id FROM merge_drop);
DELETE FROM merge_rh_conversations WHERE id IN (SELECT id FROM merge_drop);
";

const REPLACE_FAMILY: &str = "
DELETE FROM rh_tags WHERE entity_id IN (
  SELECT target_id FROM merge_drop_family
  UNION SELECT m.id FROM rh_messages m JOIN merge_drop_family d ON d.target_id = m.conversation_id
  UNION SELECT s.id FROM rh_summaries s JOIN merge_drop_family d ON d.target_id = s.conversation_id);
DELETE FROM rh_wiki_citations WHERE summary_id IN (SELECT s.id FROM rh_summaries s JOIN merge_drop_family d ON d.target_id = s.conversation_id);
DELETE FROM rh_wiki_entity_candidates WHERE summary_id IN (SELECT s.id FROM rh_summaries s JOIN merge_drop_family d ON d.target_id = s.conversation_id);
DELETE FROM rh_wiki_deferred WHERE summary_id IN (SELECT s.id FROM rh_summaries s JOIN merge_drop_family d ON d.target_id = s.conversation_id);
DELETE FROM rh_wiki_provenance WHERE source_id IN (SELECT s.id FROM rh_summaries s JOIN merge_drop_family d ON d.target_id = s.conversation_id);
DELETE FROM rh_summary_deliveries WHERE conversation_id IN (SELECT target_id FROM merge_drop_family);
UPDATE rh_summaries SET parent_id = NULL
 WHERE pipeline_version < 7 AND parent_id IN (
     SELECT s.id FROM rh_summaries s JOIN merge_drop_family d ON d.target_id = s.conversation_id);
DELETE FROM rh_summaries WHERE conversation_id IN (SELECT target_id FROM merge_drop_family);
DELETE FROM rh_conversations WHERE id IN (SELECT target_id FROM merge_drop_family);
";

const OWNERS: &str = "
UPDATE merge_rh_conversations s SET owner_user_id = t.owner_user_id
  FROM rh_conversations t WHERE t.id = s.id AND t.owner_user_id IS DISTINCT FROM s.owner_user_id;
UPDATE merge_rh_conversations s SET owner_user_id = d.target_owner
  FROM merge_dupes d WHERE d.source_id = s.id AND d.source_wins;
UPDATE merge_rh_conversations c SET owner_user_id = m.owner FROM merge_moves m WHERE c.id = m.conversation;
UPDATE merge_rh_messages x SET owner_user_id = c.owner_user_id FROM merge_rh_conversations c
 WHERE x.conversation_id = c.id AND x.owner_user_id IS DISTINCT FROM c.owner_user_id;
UPDATE merge_rh_message_chunks x SET owner_user_id = m.owner_user_id FROM merge_rh_messages m
 WHERE x.message_id = m.id AND x.owner_user_id IS DISTINCT FROM m.owner_user_id;
UPDATE merge_rh_summaries x SET owner_user_id = c.owner_user_id FROM merge_rh_conversations c
 WHERE x.conversation_id = c.id AND x.owner_user_id IS DISTINCT FROM c.owner_user_id;
UPDATE merge_rh_wiki_extractions x SET owner_user_id = s.owner_user_id FROM merge_rh_summaries s
 WHERE x.summary_id = s.id AND x.owner_user_id IS DISTINCT FROM s.owner_user_id;
UPDATE merge_rh_wiki_citations x SET owner_user_id = s.owner_user_id FROM merge_rh_summaries s
 WHERE x.summary_id = s.id AND x.owner_user_id IS DISTINCT FROM s.owner_user_id;
UPDATE merge_rh_wiki_entity_candidates x SET owner_user_id = s.owner_user_id FROM merge_rh_summaries s
 WHERE x.summary_id = s.id AND x.owner_user_id IS DISTINCT FROM s.owner_user_id;
UPDATE merge_rh_wiki_deferred x SET owner_user_id = s.owner_user_id FROM merge_rh_summaries s
 WHERE x.summary_id = s.id AND x.owner_user_id IS DISTINCT FROM s.owner_user_id;
UPDATE merge_rh_wiki_provenance x SET owner_user_id = s.owner_user_id FROM merge_rh_summaries s
 WHERE x.source_id = s.id AND x.owner_user_id IS DISTINCT FROM s.owner_user_id;
UPDATE merge_rh_conversation_placements x SET owner_user_id = c.owner_user_id FROM merge_rh_conversations c
 WHERE x.conversation_id = c.id AND x.owner_user_id IS DISTINCT FROM c.owner_user_id;
UPDATE merge_rh_session_placements x SET owner_user_id = c.owner_user_id FROM merge_rh_conversations c
 WHERE x.session_id = c.session_key AND x.owner_user_id IS DISTINCT FROM c.owner_user_id;
UPDATE merge_rh_tags x SET owner_user_id = coalesce(
    (SELECT c.owner_user_id FROM merge_rh_conversations c WHERE c.id = x.entity_id),
    (SELECT m.owner_user_id FROM merge_rh_messages m WHERE m.id = x.entity_id),
    (SELECT s.owner_user_id FROM merge_rh_summaries s WHERE s.id = x.entity_id),
    x.owner_user_id);
";

const KEEP_EXISTING_TREE: &str = "
CREATE TEMP TABLE merge_keep_existing_tree ON COMMIT DROP AS
    SELECT incoming.conversation_id
    FROM merge_rh_summaries incoming
    WHERE incoming.kind = 'session'
      AND EXISTS (SELECT 1 FROM rh_summaries current WHERE current.conversation_id = incoming.conversation_id)
      AND (NOT EXISTS (
          SELECT 1 FROM rh_summaries root WHERE root.id = incoming.id
            AND root.conversation_id = incoming.conversation_id AND root.kind = 'session'
      ) OR EXISTS (
          SELECT 1 FROM merge_rh_summaries s
          WHERE s.conversation_id = incoming.conversation_id AND s.pipeline_version = 7
            AND (s.index_needs_repair OR NOT EXISTS (SELECT 1 FROM rh_summaries t WHERE t.id = s.id))
      ));
";

const STAGED_TREE_NODES: &str = "
CREATE TEMP TABLE merge_drop_summary_nodes ON COMMIT DROP AS
    SELECT s.id, s.pipeline_version FROM merge_rh_summaries s
    WHERE s.conversation_id IN (SELECT conversation_id FROM merge_keep_existing_tree)
       OR (s.pipeline_version < 7 AND (
           EXISTS (SELECT 1 FROM rh_summaries current
                   WHERE current.conversation_id = s.conversation_id AND current.kind = 'session')
           OR EXISTS (SELECT 1 FROM merge_rh_summaries incoming
                      WHERE incoming.conversation_id = s.conversation_id AND incoming.kind = 'session'
                        AND incoming.conversation_id NOT IN (SELECT conversation_id FROM merge_keep_existing_tree))
       ));
";

const DROP_STAGED_TREE: &str = "
DELETE FROM merge_rh_summary_deliveries
    WHERE conversation_id IN (SELECT conversation_id FROM merge_keep_existing_tree);
DELETE FROM merge_rh_summary_passages WHERE record_id IN (SELECT id FROM merge_drop_summary_nodes);
DELETE FROM merge_rh_summary_sources WHERE summary_id IN (SELECT id FROM merge_drop_summary_nodes);
DELETE FROM merge_rh_wiki_extractions WHERE summary_id IN (SELECT id FROM merge_drop_summary_nodes);
DELETE FROM merge_rh_wiki_citations WHERE summary_id IN (SELECT id FROM merge_drop_summary_nodes);
DELETE FROM merge_rh_wiki_entity_candidates WHERE summary_id IN (SELECT id FROM merge_drop_summary_nodes);
DELETE FROM merge_rh_wiki_deferred WHERE summary_id IN (SELECT id FROM merge_drop_summary_nodes);
DELETE FROM merge_rh_wiki_provenance WHERE source_kind = 'summary' AND source_id IN (SELECT id FROM merge_drop_summary_nodes);
DELETE FROM merge_rh_tags WHERE entity_type = 'summary' AND entity_id IN (SELECT id FROM merge_drop_summary_nodes);
DELETE FROM merge_rh_summaries WHERE id IN (SELECT id FROM merge_drop_summary_nodes);
";

const COLLISIONS: &str = "
SELECT 'conversations', count(*) FROM merge_rh_conversations s JOIN rh_conversations t USING (id)
 WHERE (t.session_key, t.agent) IS DISTINCT FROM (s.session_key, s.agent)
UNION ALL SELECT 'messages', count(*) FROM merge_rh_messages s JOIN rh_messages t USING (id)
 WHERE (t.conversation_id, t.role) IS DISTINCT FROM (s.conversation_id, s.role)
UNION ALL SELECT 'summaries', count(*) FROM merge_rh_summaries s JOIN rh_summaries t USING (id)
 WHERE t.conversation_id IS DISTINCT FROM s.conversation_id
    OR (t.pipeline_version = 7 OR s.pipeline_version = 7) AND
       (t.kind, t.parent_id, t.summary_ordinal, t.input_key)
       IS DISTINCT FROM (s.kind, s.parent_id, s.summary_ordinal, s.input_key)
UNION ALL SELECT 'summary passages', count(*)
  FROM merge_rh_summary_passages s JOIN rh_summary_passages t USING (id)
 WHERE (t.record_id, t.ordinal, t.index_key) IS DISTINCT FROM (s.record_id, s.ordinal, s.index_key)
UNION ALL SELECT 'chunks', count(*) FROM merge_rh_message_chunks s JOIN rh_message_chunks t USING (id)
 WHERE (t.message_id, t.idx) IS DISTINCT FROM (s.message_id, s.idx)
UNION ALL SELECT 'topics', count(*) FROM merge_rh_topics s JOIN rh_topics t USING (id)
 WHERE t.kind IS DISTINCT FROM s.kind
UNION ALL SELECT 'tasks', count(*) FROM merge_rh_tasks s JOIN rh_tasks t USING (id)
 WHERE t.goal IS DISTINCT FROM s.goal
";

const TOPICS: &str = "
INSERT INTO merge_topic_map
  SELECT s.id, t.id FROM merge_rh_topics s JOIN rh_topics t ON t.kind = 'type' AND s.kind = 'type' AND t.slug = s.slug AND t.id <> s.id;
DELETE FROM merge_rh_topic_aliases a USING merge_topic_map m WHERE a.topic_id = m.source_id;
DELETE FROM merge_rh_topic_redirects r USING merge_topic_map m WHERE r.topic_id = m.source_id;
DELETE FROM merge_rh_topics s USING merge_topic_map m WHERE s.id = m.source_id;
UPDATE merge_rh_topics x SET parent_id = m.target_id FROM merge_topic_map m WHERE x.parent_id = m.source_id;
UPDATE merge_rh_topic_decisions x SET topic_id = m.target_id FROM merge_topic_map m WHERE x.topic_id = m.source_id;
UPDATE merge_rh_session_placements x SET topic_id = m.target_id FROM merge_topic_map m WHERE x.topic_id = m.source_id;
UPDATE merge_rh_conversation_placements x SET topic_id = m.target_id FROM merge_topic_map m WHERE x.topic_id = m.source_id;
UPDATE merge_rh_session_suggestions x SET topic_id = m.target_id FROM merge_topic_map m WHERE x.topic_id = m.source_id;
UPDATE merge_rh_session_suggestions x SET parent_id = m.target_id FROM merge_topic_map m WHERE x.parent_id = m.source_id;
UPDATE merge_rh_session_startup x SET topic_id = m.target_id FROM merge_topic_map m WHERE x.topic_id = m.source_id;
UPDATE merge_rh_filing_corrections x SET from_topic = m.target_id FROM merge_topic_map m WHERE x.from_topic = m.source_id;
UPDATE merge_rh_filing_corrections x SET to_topic = m.target_id FROM merge_topic_map m WHERE x.to_topic = m.source_id;
UPDATE merge_rh_wiki_topics x SET topic_id = m.target_id FROM merge_topic_map m WHERE x.topic_id = m.source_id;
UPDATE merge_rh_wiki_deferred x SET applied_topic = m.target_id FROM merge_topic_map m WHERE x.applied_topic = m.source_id;
UPDATE merge_rh_tags x SET entity_id = m.target_id FROM merge_topic_map m
 WHERE x.entity_type = 'topic' AND x.entity_id = m.source_id;
UPDATE merge_rh_wiki_topics s SET topic_id = NULL
 WHERE s.topic_id IS NOT NULL AND EXISTS (SELECT 1 FROM rh_wiki_topics t WHERE t.topic_id = s.topic_id);
";

const RENAME_TOPICS: &str = "
UPDATE merge_rh_topics s SET slug = s.slug || '-' || $1
 WHERE EXISTS (SELECT 1 FROM rh_topics t WHERE t.kind = s.kind AND t.slug = s.slug AND t.id <> s.id)
";

const ALIASES: &str = "
DELETE FROM merge_rh_topic_aliases a
 WHERE EXISTS (SELECT 1 FROM rh_topic_aliases t WHERE t.kind = a.kind AND t.alias_key = a.alias_key AND t.topic_id <> a.topic_id)
";

const PAGE_MAP: &str = "
INSERT INTO merge_slug_map
  SELECT slug, $1 || '-' || slug FROM merge_rh_wiki_topics WHERE left(slug, length($1) + 1) <> $1 || '-'";

const PAGES: &str = "
UPDATE merge_rh_wiki_topics w SET related = ARRAY(
    SELECT coalesce(m.new, r.slug) FROM unnest(w.related) WITH ORDINALITY AS r(slug, n)
      LEFT JOIN merge_slug_map m ON m.old = r.slug ORDER BY r.n)
 WHERE w.related IS NOT NULL;
UPDATE merge_rh_wiki_extractions w SET topics_touched = ARRAY(
    SELECT coalesce(m.new, r.slug) FROM unnest(w.topics_touched) WITH ORDINALITY AS r(slug, n)
      LEFT JOIN merge_slug_map m ON m.old = r.slug ORDER BY r.n)
 WHERE w.topics_touched IS NOT NULL;
UPDATE merge_rh_wiki_topics x SET slug = m.new FROM merge_slug_map m WHERE x.slug = m.old;
UPDATE merge_rh_wiki_citations x SET topic_slug = m.new FROM merge_slug_map m WHERE x.topic_slug = m.old;
UPDATE merge_rh_wiki_provenance x SET topic_slug = m.new FROM merge_slug_map m WHERE x.topic_slug = m.old;
UPDATE merge_rh_wiki_redirects x SET to_slug = m.new FROM merge_slug_map m WHERE x.to_slug = m.old;
";

const PAGE_SCOPE: &str = "
UPDATE merge_rh_wiki_topics w SET scope = $1, local_slug = coalesce(w.local_slug, m.old)
  FROM merge_slug_map m WHERE w.slug = m.new";

const SCOPE_ROW: &str = "
INSERT INTO rh_wiki_scopes (scope, owner_user_id, visibility, key_prefix, directory)
VALUES ($1, $1, 'private', $1 || '-', 'users/' || $1) ON CONFLICT DO NOTHING";

const REDIRECTS: &str = "
UPDATE merge_rh_wiki_redirects x SET from_slug = $1 || '-' || x.from_slug
 WHERE left(x.from_slug, length($1) + 1) <> $1 || '-'";

async fn batch(
    conn: &mut PgConnection,
    sql: &'static str,
    step: &'static str,
) -> Result<(), Error> {
    sqlx::raw_sql(sql)
        .execute(&mut *conn)
        .await
        .map(drop)
        .map_err(|error| Error::operation(step, error))
}

async fn bound(
    conn: &mut PgConnection,
    sql: &'static str,
    owner: &str,
    step: &'static str,
) -> Result<u64, Error> {
    sqlx::query(sql)
        .bind(owner)
        .execute(&mut *conn)
        .await
        .map(|done| done.rows_affected())
        .map_err(|error| Error::operation(step, error))
}

async fn count(conn: &mut PgConnection, sql: &str) -> Result<u64, Error> {
    let found: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(&mut *conn)
        .await
        .map_err(|error| Error::operation("merge count", error))?;
    u64::try_from(found).map_err(|error| Error::Merge(format!("merge count: {error}")))
}

async fn duplicates(conn: &mut PgConnection, report: &mut MergeReport) -> Result<(), Error> {
    let found: Vec<(String, bool)> =
        sqlx::query_as("SELECT session_key, source_wins FROM merge_dupes ORDER BY session_key")
            .fetch_all(&mut *conn)
            .await
            .map_err(|error| Error::operation("merge duplicates", error))?;
    for (key, source_wins) in found {
        if source_wins {
            report.replaced.push(key);
        } else {
            report.kept_family.push(key);
        }
    }
    report.dropped_summary_trees = sqlx::query_as(
        "SELECT d.source_id AS conversation, d.session_key, incoming.id AS incoming_session,
                existing.id AS destination_session
         FROM merge_dupes d JOIN merge_rh_summaries incoming ON incoming.conversation_id = d.source_id
         LEFT JOIN rh_summaries existing ON existing.conversation_id = d.target_id AND existing.kind = 'session'
         WHERE NOT d.source_wins AND incoming.kind = 'session'
         ORDER BY d.session_key, incoming.id",
    ).fetch_all(&mut *conn).await.map_err(|error| Error::operation("merge dropped summary trees", error))?;
    batch(
        conn,
        "INSERT INTO merge_drop SELECT source_id FROM merge_dupes WHERE NOT source_wins;
         CREATE TEMP TABLE merge_drop_family ON COMMIT DROP AS
           SELECT target_id FROM merge_dupes WHERE source_wins;",
        "merge duplicates",
    )
    .await?;
    batch(conn, DROP_STAGED, "merge duplicates").await?;
    batch(conn, REPLACE_FAMILY, "merge replace").await
}

async fn refuse_collisions(conn: &mut PgConnection) -> Result<(), Error> {
    let found: Vec<(String, i64)> = sqlx::query_as(COLLISIONS)
        .fetch_all(&mut *conn)
        .await
        .map_err(|error| Error::operation("merge collisions", error))?;
    let clashing: Vec<String> = found
        .into_iter()
        .filter(|(_, rows)| *rows > 0)
        .map(|(table, rows)| format!("{rows} {table}"))
        .collect();
    if clashing.is_empty() {
        Ok(())
    } else {
        Err(Error::Merge(format!(
            "rows share an id with different family rows: {}",
            clashing.join(", ")
        )))
    }
}

pub(super) async fn run(
    conn: &mut PgConnection,
    request: &MergeRequest,
    report: &mut MergeReport,
) -> Result<(), Error> {
    let owner = request.owner.as_str();
    batch(conn, SETUP, "merge setup").await?;
    let (ids, owners): (Vec<Uuid>, Vec<String>) = request
        .moves
        .iter()
        .map(|(id, owner)| (*id, owner.as_str().to_owned()))
        .unzip();
    sqlx::query("INSERT INTO merge_moves SELECT * FROM unnest($1::uuid[], $2::text[])")
        .bind(&ids)
        .bind(&owners)
        .execute(&mut *conn)
        .await
        .map_err(|error| Error::operation("merge moves", error))?;
    for table in super::Table::ALL.into_iter().filter(|table| table.owned()) {
        let stage = staged(table.name());
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {stage} SET owner_user_id = $1 WHERE owner_user_id IS DISTINCT FROM $1"
        )))
        .bind(owner)
        .execute(&mut *conn)
        .await
        .map_err(|error| Error::operation("merge owners", error))?;
    }
    sqlx::query("UPDATE merge_rh_topics SET owner_user_id = $1 WHERE owner_user_id IS NOT NULL")
        .bind(owner)
        .execute(&mut *conn)
        .await
        .map_err(|error| Error::operation("merge owners", error))?;
    duplicates(conn, report).await?;
    report.moved_conversations = count(
        conn,
        "SELECT count(*) FROM merge_rh_conversations c JOIN merge_moves m ON m.conversation = c.id",
    )
    .await?;
    batch(conn, OWNERS, "merge owners").await?;
    reconcile_summary_trees(conn, report).await?;
    refuse_collisions(conn).await?;
    batch(conn, TOPICS, "merge topics").await?;
    report.mapped_type_topics = count(conn, "SELECT count(*) FROM merge_topic_map").await?;
    for _ in 0..3 {
        let renamed = bound(conn, RENAME_TOPICS, owner, "merge topics").await?;
        report.renamed_topics += renamed;
        if renamed == 0 {
            break;
        }
    }
    report.dropped_aliases = sqlx::query(ALIASES)
        .execute(&mut *conn)
        .await
        .map(|done| done.rows_affected())
        .map_err(|error| Error::operation("merge aliases", error))?;
    report.prefixed_pages = bound(conn, PAGE_MAP, owner, "merge pages").await?;
    batch(conn, PAGES, "merge pages").await?;
    bound(conn, REDIRECTS, owner, "merge pages").await?;
    bound(conn, SCOPE_ROW, owner, "merge pages").await?;
    bound(conn, PAGE_SCOPE, owner, "merge pages").await?;
    Ok(())
}

async fn reconcile_summary_trees(
    conn: &mut PgConnection,
    report: &mut MergeReport,
) -> Result<(), Error> {
    batch(conn, KEEP_EXISTING_TREE, "merge existing summary trees").await?;
    report.dropped_summary_trees.extend(
        sqlx::query_as(
            "SELECT incoming.conversation_id AS conversation, c.session_key,
                    incoming.id AS incoming_session, existing.id AS destination_session
             FROM merge_keep_existing_tree keep
             JOIN merge_rh_summaries incoming ON incoming.conversation_id = keep.conversation_id
             JOIN merge_rh_conversations c ON c.id = keep.conversation_id
             LEFT JOIN rh_summaries existing ON existing.conversation_id = keep.conversation_id
                                              AND existing.kind = 'session'
             WHERE incoming.kind = 'session'
             ORDER BY c.session_key, incoming.id",
        )
        .fetch_all(&mut *conn)
        .await
        .map_err(|error| Error::operation("merge dropped summary trees", error))?,
    );
    batch(conn, STAGED_TREE_NODES, "merge select discarded summaries").await?;
    report.dropped_legacy_summaries = count(
        conn,
        "SELECT count(*) FROM merge_drop_summary_nodes WHERE pipeline_version < 7",
    )
    .await?;
    batch(conn, DROP_STAGED_TREE, "merge drop staged summary trees").await?;
    Ok(())
}

const REFRESH_SETTLEMENT: &str = "
-- Reopen existing sessions and keep summarized_through and their current trees.
changed AS (
    UPDATE rh_conversations c SET last_summary_input_at = clock_timestamp()
     WHERE c.id IN (SELECT conversation_id FROM inserted)
       AND (NOT EXISTS (SELECT 1 FROM merge_new_conversations new WHERE new.id = c.id)
            OR c.id IN (SELECT source_id FROM merge_dupes WHERE source_wins AND target_live))
    RETURNING c.id, c.last_summary_input_at
), refreshed AS (
    INSERT INTO rh_summary_state AS state
        (conversation_id, input_revision, last_input_at, unsettled_since, historical_only)
    SELECT id, 1, last_summary_input_at, last_summary_input_at, false FROM changed
    ON CONFLICT (conversation_id) DO UPDATE SET
        input_revision = state.input_revision + 1,
        last_input_at = excluded.last_input_at,
        unsettled_since = coalesce(state.unsettled_since, excluded.unsettled_since),
        settled_at = NULL,
        failed_revision = NULL,
        historical_only = false
)";

pub(super) async fn insert(
    conn: &mut PgConnection,
    table: Table,
    columns: &[String],
) -> Result<u64, Error> {
    if columns.is_empty() {
        return Ok(0);
    }
    let name = table.name();
    let stage = staged(name);
    let list = columns
        .iter()
        .map(|column| format!("\"{column}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let conflict = if table == Table::Summaries {
        "(id)"
    } else {
        ""
    };
    let insert = format!(
        "INSERT INTO public.{name} ({list}) SELECT {list} FROM {stage} ON CONFLICT {conflict} DO NOTHING"
    );
    if table == Table::Messages {
        return count(
            conn,
            &format!(
                "WITH inserted AS ({insert} RETURNING conversation_id),
                 {REFRESH_SETTLEMENT} SELECT count(*) FROM inserted"
            ),
        )
        .await;
    }
    let statement = if table == Table::Conversations {
        format!(
            "WITH inserted AS ({insert} RETURNING id)
             INSERT INTO merge_new_conversations SELECT id FROM inserted"
        )
    } else {
        insert
    };
    sqlx::query(sqlx::AssertSqlSafe(statement))
        .execute(&mut *conn)
        .await
        .map(|done| done.rows_affected())
        .map_err(|error| Error::operation("merge insert", error))
}
