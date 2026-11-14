#![cfg(test)]

use chrono::DateTime;
use chrono::Utc;
use kernel::tenancy::Scope;
use kernel::wiki::entity_pass::EntityStore;
use memory_postgres::PostgresMemory;
use pgvector::HalfVector;
use sqlx::PgPool;
use sqlx::Row;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_merge_moves_the_newer_page_into_the_older_one(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(merge_case(pool)).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_merge_removes_the_absorbed_page_file(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(merge_files(pool)).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_merge_binds_an_unbound_survivor_page(pool: PgPool) -> TestResult {
    Scope::Maintenance.run(bind_survivor(pool)).await
}

async fn merge_case(pool: PgPool) -> TestResult {
    let conversation = seed_conversation(&pool).await?;
    let older_summary = seed_leaf(&pool, conversation, at("2026-10-01T00:00:00Z")).await?;
    let newer_summary = seed_leaf(&pool, conversation, at("2026-11-01T00:00:00Z")).await?;
    let shared = seed_leaf(&pool, conversation, at("2026-10-15T00:00:00Z")).await?;
    let kind = seed_type(&pool).await?;
    let older = Uuid::new_v4();
    let younger = Uuid::new_v4();
    seed_entity(
        &pool,
        older,
        "box_a",
        "box_a",
        kind,
        at("2026-10-01T00:00:00Z"),
    )
    .await?;
    seed_entity(&pool, younger, "g9", "G9", kind, at("2026-11-01T00:00:00Z")).await?;
    seed_alias(&pool, younger, "box9").await?;
    let pages = Pages {
        older,
        younger,
        conversation,
        older_summary,
        newer_summary,
        shared,
    };
    seed_facts(&pool, &pages).await?;
    seed_history(&pool, &pages).await?;
    seed_mark(&pool, older, older_summary, at("2026-10-01T00:00:00Z")).await?;
    seed_mark(&pool, younger, newer_summary, at("2026-11-08T00:00:00Z")).await?;
    seed_embedding(&pool, older, at("2026-10-01T00:00:00Z")).await?;
    seed_embedding(&pool, younger, at("2026-11-08T00:00:00Z")).await?;
    seed_type_seen(&pool, younger, newer_summary).await?;
    seed_placement(&pool, younger).await?;
    seed_pages(&pool, older, younger, newer_summary).await?;
    let memory = PostgresMemory::new(pool.clone());
    let now = at("2026-11-12T00:00:00Z");
    let done = memory
        .merge_entities(younger, older, now)
        .await?
        .ok_or("merge returned nothing")?;
    assert!(done.performed);
    assert_eq!(done.survivor, older);
    assert_eq!(done.absorbed, younger);
    assert_eq!(done.survivor_name, "box_a");
    assert_eq!(done.absorbed_name, "G9");
    assert_rows(&pool, older, younger).await?;
    assert_pages(&pool, older, younger, newer_summary).await?;
    let again = memory
        .merge_entities(younger, older, now)
        .await?
        .ok_or("second merge returned nothing")?;
    assert!(!again.performed);
    assert_eq!(again.survivor, older);
    Ok(())
}

async fn bind_survivor(pool: PgPool) -> TestResult {
    let kind = seed_type(&pool).await?;
    let older = Uuid::new_v4();
    let younger = Uuid::new_v4();
    seed_entity(
        &pool,
        older,
        "box_a",
        "box_a",
        kind,
        at("2026-10-01T00:00:00Z"),
    )
    .await?;
    seed_entity(&pool, younger, "g9", "G9", kind, at("2026-11-01T00:00:00Z")).await?;
    unbound_page(&pool, "box_a", "older host").await?;
    unbound_page(&pool, "g9", "newer host").await?;
    let memory = PostgresMemory::new(pool.clone());
    let done = memory
        .merge_entities(younger, older, at("2026-11-12T00:00:00Z"))
        .await?
        .ok_or("merge returned nothing")?;
    assert!(done.performed);
    assert_eq!(done.survivor, older);
    let bound: Uuid =
        sqlx::query_scalar("SELECT topic_id FROM rh_wiki_topics WHERE slug = 'box_a'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(bound, older);
    Ok(())
}

async fn unbound_page(pool: &PgPool, slug: &str, state: &str) -> Result<(), sqlx::Error> {
    let mut tx = deferred(pool).await?;
    sqlx::query(
        "INSERT INTO rh_wiki_topics (slug, title, current_state, owner_user_id)
         VALUES ($1, $1, $2, 'owner')",
    )
    .bind(slug)
    .bind(state)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn merge_files(pool: PgPool) -> TestResult {
    let kind = seed_type(&pool).await?;
    let older = Uuid::new_v4();
    let younger = Uuid::new_v4();
    seed_entity(
        &pool,
        older,
        "box_a",
        "box_a",
        kind,
        at("2026-10-01T00:00:00Z"),
    )
    .await?;
    seed_entity(&pool, younger, "g9", "G9", kind, at("2026-11-01T00:00:00Z")).await?;
    page(&pool, "box_a", "older host", older).await?;
    page(&pool, "g9", "newer host", younger).await?;
    let directory = tempfile::tempdir()?;
    let topics = directory.path().join("topics");
    std::fs::create_dir(&topics)?;
    std::fs::write(topics.join("box_a.md"), "# box_a\n\nolder host\n")?;
    std::fs::write(topics.join("g9.md"), "# G9\n\nnewer host\n")?;
    git(directory.path(), &["init", "-b", "main"])?;
    git(
        directory.path(),
        &["config", "user.email", "wiki@rivetos.dev"],
    )?;
    git(directory.path(), &["config", "user.name", "RivetOS Wiki"])?;
    git(directory.path(), &["add", "topics"])?;
    git(directory.path(), &["commit", "-m", "seed"])?;
    let memory =
        PostgresMemory::new(pool.clone()).with_wiki_directory(directory.path().to_path_buf());
    let done = memory
        .merge_entities(younger, older, at("2026-11-12T00:00:00Z"))
        .await?
        .ok_or("merge returned nothing")?;
    assert!(done.performed);
    assert_eq!(done.absorbed, younger);
    assert!(!topics.join("g9.md").exists());
    assert!(topics.join("box_a.md").exists());
    let subject = git_text(directory.path(), &["log", "-1", "--pretty=%s"])?;
    assert_eq!(subject.trim(), "entity merge: remove g9");
    let redirect: String =
        sqlx::query_scalar("SELECT to_slug FROM rh_wiki_redirects WHERE from_slug = 'g9'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(redirect, "box_a");
    Ok(())
}

fn git(directory: &std::path::Path, args: &[&str]) -> TestResult {
    let output = std::process::Command::new("git")
        .current_dir(directory)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(())
}

fn git_text(
    directory: &std::path::Path,
    args: &[&str],
) -> Result<String, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("git")
        .current_dir(directory)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

async fn assert_rows(pool: &PgPool, older: Uuid, younger: Uuid) -> TestResult {
    assert_eq!(
        count(
            pool,
            "SELECT count(*) FROM rh_topics WHERE id = $1",
            younger
        )
        .await?,
        0
    );
    let mut aliases = text_list(
        pool,
        "SELECT alias FROM rh_topic_aliases WHERE topic_id = $1 AND kind = 'entity' ORDER BY alias",
        older,
    )
    .await?;
    aliases.sort();
    assert_eq!(aliases, vec!["G9".to_owned(), "box9".to_owned()]);
    let facts = text_pairs(
        pool,
        "SELECT f.key, c.value
           FROM rh_entity_fact_claims c
           JOIN rh_entity_facts f ON f.id = c.fact_id
          WHERE f.topic_id = $1
            AND (c.at, c.recorded_at, c.id) = (
                SELECT c2.at, c2.recorded_at, c2.id
                  FROM rh_entity_fact_claims c2
                  JOIN rh_entity_facts f2 ON f2.id = c2.fact_id
                 WHERE f2.topic_id = f.topic_id AND f2.key = f.key
                 ORDER BY c2.at DESC, c2.recorded_at DESC, c2.id DESC
                 LIMIT 1
            )
          ORDER BY f.key",
        older,
    )
    .await?;
    assert_eq!(
        facts,
        vec![
            ("ip".to_owned(), "new-ip".to_owned()),
            ("model".to_owned(), "Qwen 27B".to_owned()),
            ("role".to_owned(), "serves qwen".to_owned()),
        ]
    );
    assert_eq!(
        count(
            pool,
            "SELECT count(*) FROM rh_entity_fact_claims c
              JOIN rh_entity_facts f ON f.id = c.fact_id
             WHERE f.topic_id = $1 AND f.key = 'role' AND c.value = 'compute node'",
            older,
        )
        .await?,
        1
    );
    let mut history = text_list(
        pool,
        "SELECT sentence FROM rh_entity_history WHERE entity_id = $1",
        older,
    )
    .await?;
    history.sort();
    assert_eq!(
        history,
        vec![
            "Called it G9.".to_owned(),
            "Installed box_a.".to_owned(),
            "kept line".to_owned(),
        ]
    );
    Ok(())
}

async fn assert_pages(
    pool: &PgPool,
    older: Uuid,
    younger: Uuid,
    newer_summary: Uuid,
) -> TestResult {
    let embedded: DateTime<Utc> =
        sqlx::query_scalar("SELECT embedded_at FROM rh_entity_embeddings WHERE entity_id = $1")
            .bind(older)
            .fetch_one(pool)
            .await?;
    assert_eq!(embedded, at("2026-11-08T00:00:00Z"));
    let mark: Uuid =
        sqlx::query_scalar("SELECT summary_id FROM rh_entity_applied WHERE entity_id = $1")
            .bind(older)
            .fetch_one(pool)
            .await?;
    assert_eq!(mark, newer_summary);
    assert_eq!(
        count(
            pool,
            "SELECT count(*) FROM rh_entity_applied WHERE entity_id = $1",
            younger
        )
        .await?,
        0
    );
    let redirect: String =
        sqlx::query_scalar("SELECT to_slug FROM rh_wiki_redirects WHERE from_slug = 'g9'")
            .fetch_one(pool)
            .await?;
    assert_eq!(redirect, "box_a");
    let state: String =
        sqlx::query_scalar("SELECT current_state FROM rh_wiki_topics WHERE slug = 'box_a'")
            .fetch_one(pool)
            .await?;
    assert_eq!(state, "older host");
    assert_eq!(
        count_slug(
            pool,
            "SELECT count(*) FROM rh_wiki_topics WHERE slug = $1",
            "g9"
        )
        .await?,
        0
    );
    assert_eq!(
        count_slug(
            pool,
            "SELECT count(*) FROM rh_wiki_citations WHERE topic_slug = $1",
            "box_a"
        )
        .await?,
        1
    );
    let placed: Uuid = sqlx::query_scalar(
        "SELECT topic_id FROM rh_session_placements WHERE session_id = 'claude-code:entity-merge'",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(placed, older);
    let dangling = references_to(pool, younger).await?;
    assert!(dangling.is_empty(), "{dangling:?}");
    Ok(())
}

struct Pages {
    older: Uuid,
    younger: Uuid,
    conversation: Uuid,
    older_summary: Uuid,
    newer_summary: Uuid,
    shared: Uuid,
}

struct FactRow<'a> {
    entity: Uuid,
    attribute: &'a str,
    value: &'a str,
    as_of: DateTime<Utc>,
    summary: Uuid,
    conversation: Uuid,
}

async fn seed_facts(pool: &PgPool, pages: &Pages) -> TestResult {
    fact(
        pool,
        FactRow {
            entity: pages.older,
            attribute: "role",
            value: "compute node",
            as_of: at("2026-10-02T00:00:00Z"),
            summary: pages.older_summary,
            conversation: pages.conversation,
        },
    )
    .await?;
    fact(
        pool,
        FactRow {
            entity: pages.younger,
            attribute: "role",
            value: "serves qwen",
            as_of: at("2026-11-02T00:00:00Z"),
            summary: pages.newer_summary,
            conversation: pages.conversation,
        },
    )
    .await?;
    fact(
        pool,
        FactRow {
            entity: pages.older,
            attribute: "ip",
            value: "old-ip",
            as_of: at("2026-10-03T00:00:00Z"),
            summary: pages.shared,
            conversation: pages.conversation,
        },
    )
    .await?;
    fact(
        pool,
        FactRow {
            entity: pages.younger,
            attribute: "ip",
            value: "new-ip",
            as_of: at("2026-11-03T00:00:00Z"),
            summary: pages.shared,
            conversation: pages.conversation,
        },
    )
    .await?;
    fact(
        pool,
        FactRow {
            entity: pages.younger,
            attribute: "model",
            value: "Qwen 27B",
            as_of: at("2026-11-02T00:00:00Z"),
            summary: pages.newer_summary,
            conversation: pages.conversation,
        },
    )
    .await?;
    Ok(())
}

async fn seed_history(pool: &PgPool, pages: &Pages) -> TestResult {
    history(
        pool,
        pages.older,
        pages.older_summary,
        pages.conversation,
        "Installed box_a.",
    )
    .await?;
    history(
        pool,
        pages.younger,
        pages.newer_summary,
        pages.conversation,
        "Called it G9.",
    )
    .await?;
    history(
        pool,
        pages.older,
        pages.shared,
        pages.conversation,
        "kept line",
    )
    .await?;
    history(
        pool,
        pages.younger,
        pages.shared,
        pages.conversation,
        "dropped line",
    )
    .await?;
    Ok(())
}

async fn seed_pages(pool: &PgPool, older: Uuid, younger: Uuid, summary: Uuid) -> TestResult {
    page(pool, "box_a", "older host", older).await?;
    page(pool, "g9", "younger host", younger).await?;
    let mut tx = deferred(pool).await?;
    sqlx::query(
        "INSERT INTO rh_wiki_citations (topic_slug, summary_id, kind, note, owner_user_id)
         VALUES ('g9', $1, 'summary', 'from g9', 'owner')",
    )
    .bind(summary)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO rh_wiki_provenance
            (topic_slug, source_kind, source_id, owner_user_id)
         VALUES ('g9', 'summary', $1, 'owner')",
    )
    .bind(summary)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn references_to(pool: &PgPool, id: Uuid) -> Result<Vec<String>, sqlx::Error> {
    let columns = sqlx::query_as::<_, RefColumn>(
        "SELECT c.relname AS table_name, a.attname AS column_name
           FROM pg_attribute a
           JOIN pg_class c ON c.oid = a.attrelid
           JOIN pg_namespace n ON n.oid = c.relnamespace
           JOIN pg_type t ON t.oid = a.atttypid
          WHERE n.nspname = current_schema()
            AND c.relkind = 'r'
            AND a.attnum > 0
            AND NOT a.attisdropped
            AND t.typname = 'uuid'
            AND a.attname IN ('entity_id', 'topic_id', 'from_topic', 'to_topic', 'parent_id')",
    )
    .fetch_all(pool)
    .await?;
    let mut hits = Vec::new();
    for column in columns {
        let Some(sql) = count_sql(&column) else {
            continue;
        };
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_one(pool)
            .await?;
        let refs: i64 = row.try_get("refs")?;
        if refs > 0 {
            hits.push(format!(
                "{}.{}={refs}",
                column.table_name, column.column_name
            ));
        }
    }
    Ok(hits)
}

fn count_sql(column: &RefColumn) -> Option<String> {
    let table = quote(&column.table_name)?;
    let field = quote(&column.column_name)?;
    Some(format!(
        "SELECT count(*)::bigint AS refs FROM {table} WHERE {field} = $1"
    ))
}

fn quote(name: &str) -> Option<String> {
    name.chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        .then(|| format!("\"{name}\""))
}

#[derive(Debug, sqlx::FromRow)]
struct RefColumn {
    table_name: String,
    column_name: String,
}

async fn seed_conversation(pool: &PgPool) -> Result<Uuid, sqlx::Error> {
    let mut tx = deferred(pool).await?;
    let id = sqlx::query_scalar(
        "INSERT INTO rh_conversations (session_key, agent, channel, title, owner_user_id)
         VALUES ('claude-code:entity-merge', 'rivet', 'cli', 'merge', 'owner') RETURNING id",
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

async fn seed_leaf(
    pool: &PgPool,
    conversation: Uuid,
    start: DateTime<Utc>,
) -> Result<Uuid, sqlx::Error> {
    let mut tx = deferred(pool).await?;
    let id = sqlx::query_scalar(
        "INSERT INTO rh_summaries (conversation_id, content, kind, earliest_at, latest_at, owner_user_id)
         VALUES ($1, 'leaf', 'leaf', $2, $3, 'owner') RETURNING id",
    )
    .bind(conversation)
    .bind(start)
    .bind(start + chrono::Duration::hours(1))
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

async fn seed_type(pool: &PgPool) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar("SELECT id FROM rh_topics WHERE kind = 'type' AND slug = 'hosts'")
        .fetch_one(pool)
        .await
}

async fn seed_entity(
    pool: &PgPool,
    id: Uuid,
    slug: &str,
    name: &str,
    parent: Uuid,
    created: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let mut tx = deferred(pool).await?;
    sqlx::query(
        "INSERT INTO rh_topics (id, kind, slug, name, parent_id, created_at, updated_at, owner_user_id)
         VALUES ($1, 'entity', $2, $3, $4, $5, $5, 'owner')",
    )
    .bind(id)
    .bind(slug)
    .bind(name)
    .bind(parent)
    .bind(created)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn seed_alias(pool: &PgPool, entity: Uuid, alias: &str) -> Result<(), sqlx::Error> {
    let mut tx = deferred(pool).await?;
    sqlx::query(
        "INSERT INTO rh_topic_aliases (topic_id, alias, alias_key, kind) VALUES ($1, $2, $2, 'entity')",
    )
    .bind(entity)
    .bind(alias)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn fact(pool: &PgPool, row: FactRow<'_>) -> Result<(), sqlx::Error> {
    let mut tx = deferred(pool).await?;
    sqlx::query(
        "WITH fact AS (
            INSERT INTO rh_entity_facts (id, topic_id, key, created_at)
            VALUES ($1, $2, $3, $4)
            RETURNING id
         )
         INSERT INTO rh_entity_fact_claims
            (id, fact_id, value, actor, pinned, session_id, summary_id, at, recorded_at, owner_user_id)
         SELECT $5, fact.id, $6, 'wiki-writer', false,
                (SELECT session_key FROM rh_conversations WHERE id = $7),
                $8, $4, $4, 'owner'
           FROM fact",
    )
    .bind(Uuid::new_v4())
    .bind(row.entity)
    .bind(row.attribute)
    .bind(row.as_of)
    .bind(Uuid::new_v4())
    .bind(row.value)
    .bind(row.conversation)
    .bind(row.summary)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn history(
    pool: &PgPool,
    entity: Uuid,
    summary: Uuid,
    conversation: Uuid,
    sentence: &str,
) -> Result<(), sqlx::Error> {
    let mut tx = deferred(pool).await?;
    sqlx::query(
        "INSERT INTO rh_entity_history
            (entity_id, summary_id, conversation_id, session_on, sentence, owner_user_id)
         VALUES ($1, $2, $3, $4, $5, 'owner')",
    )
    .bind(entity)
    .bind(summary)
    .bind(conversation)
    .bind(at("2026-11-01T00:00:00Z").date_naive())
    .bind(sentence)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn seed_mark(
    pool: &PgPool,
    entity: Uuid,
    summary: Uuid,
    through: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let mut tx = deferred(pool).await?;
    sqlx::query(
        "INSERT INTO rh_entity_applied (entity_id, applied_through, summary_id, owner_user_id)
         VALUES ($1, $2, $3, 'owner')",
    )
    .bind(entity)
    .bind(through)
    .bind(summary)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn seed_embedding(
    pool: &PgPool,
    entity: Uuid,
    embedded_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let mut tx = deferred(pool).await?;
    sqlx::query(
        "INSERT INTO rh_entity_embeddings (entity_id, embedding, embedded_at, owner_user_id)
         VALUES ($1, $2, $3, 'owner')",
    )
    .bind(entity)
    .bind(HalfVector::from_f32_slice(&vec![0.25_f32; 1024]))
    .bind(embedded_at)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn seed_type_seen(pool: &PgPool, entity: Uuid, summary: Uuid) -> Result<(), sqlx::Error> {
    let mut tx = deferred(pool).await?;
    sqlx::query(
        "INSERT INTO rh_entity_type_seen (entity_id, summary_id, type_name, observed_at, owner_user_id)
         VALUES ($1, $2, 'Hosts', $3, 'owner')",
    )
    .bind(entity)
    .bind(summary)
    .bind(at("2026-11-01T00:00:00Z"))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn seed_placement(pool: &PgPool, entity: Uuid) -> Result<(), sqlx::Error> {
    let mut tx = deferred(pool).await?;
    sqlx::query(
        "INSERT INTO rh_session_placements (session_id, topic_id, placed_by, placed_at, owner_user_id)
         VALUES ('claude-code:entity-merge', $1, 'you', $2, 'owner')",
    )
    .bind(entity)
    .bind(at("2026-11-01T00:00:00Z"))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn page(pool: &PgPool, slug: &str, state: &str, entity: Uuid) -> Result<(), sqlx::Error> {
    let mut tx = deferred(pool).await?;
    sqlx::query(
        "INSERT INTO rh_wiki_topics (slug, title, current_state, topic_id, owner_user_id)
         VALUES ($1, $1, $2, $3, 'owner')",
    )
    .bind(slug)
    .bind(state)
    .bind(entity)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn count(pool: &PgPool, sql: &'static str, id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(sql).bind(id).fetch_one(pool).await
}

async fn count_slug(pool: &PgPool, sql: &'static str, slug: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(sql).bind(slug).fetch_one(pool).await
}

async fn text_list(pool: &PgPool, sql: &'static str, id: Uuid) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(sql).bind(id).fetch_all(pool).await
}

async fn text_pairs(
    pool: &PgPool,
    sql: &'static str,
    id: Uuid,
) -> Result<Vec<(String, String)>, sqlx::Error> {
    sqlx::query_as(sql).bind(id).fetch_all(pool).await
}

async fn deferred(
    pool: &PgPool,
) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('rivet.defer_embed_enqueue', 'on', true)")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text).map_or(DateTime::UNIX_EPOCH, |time| time.with_timezone(&Utc))
}
