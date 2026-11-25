use kernel::owner::Owner;
use kernel::topics::{TopicId, TopicStore};
use memory_postgres::PostgresMemory;
use memory_sqlite::{SqliteError, SqliteStore};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use super::super::{Direction, run, verify};
use super::{TestResult, store};

async fn copy(
    pool: &PgPool,
    local: &SqliteStore,
    principal: &str,
    direction: Direction,
) -> Result<super::super::TransferReport, Box<dyn std::error::Error>> {
    let owner = Owner::new(principal.to_owned())?;
    Ok(run(
        pool,
        local,
        [&owner, &owner],
        direction,
        (false, None),
        &CancellationToken::new(),
    )
    .await?)
}

async fn strings(store: &SqliteStore, sql: &'static str) -> Result<Vec<String>, SqliteError> {
    store
        .with_conn(|conn| {
            conn.prepare(sql)
                .and_then(|mut stmt| stmt.query_map([], |r| r.get(0))?.collect())
                .map_err(SqliteError::Pragma)
        })
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn carries_own_claims_on_owned_topics_and_global_types(pool: PgPool) -> TestResult {
    sqlx::raw_sql(
        "INSERT INTO rh_topics (id, kind, slug, name, owner_user_id, created_at, updated_at) VALUES
            (gen_random_uuid(), 'work', 'alice-topic', 'Alice topic', 'alice', now(), now()),
            (gen_random_uuid(), 'work', 'bob-topic', 'Second topic', 'bob', now(), now());
         INSERT INTO rh_entity_facts (id, topic_id, key, created_at)
         SELECT gen_random_uuid(), id, 'example', now() FROM rh_topics
         WHERE slug IN ('alice-topic', 'bob-topic', 'software');
         INSERT INTO rh_entity_fact_claims (id, fact_id, value, actor, owner_user_id, at, recorded_at)
         SELECT gen_random_uuid(), f.id, who.owner || ' on ' || t.slug, 'you', who.owner, now(), now()
         FROM rh_entity_facts f JOIN rh_topics t ON t.id = f.topic_id
         CROSS JOIN (VALUES ('alice'), ('bob')) who(owner)",
    )
    .execute(&pool)
    .await?;
    let directory = tempfile::tempdir()?;
    for who in ["alice", "bob"] {
        let local = store(&directory, who)?;
        let report = copy(&pool, &local, who, Direction::ToSqlite).await?;
        assert_eq!(report.omissions.claims_on_other_topics, 1);
        assert_eq!(
            strings(
                &local,
                "SELECT value FROM rh_entity_fact_claims ORDER BY value"
            )
            .await?,
            vec![
                format!("{who} on {who}-topic"),
                format!("{who} on software")
            ]
        );
        assert_eq!(
            strings(
                &local,
                "SELECT slug FROM rh_topics WHERE kind = 'work' ORDER BY slug"
            )
            .await?,
            vec![format!("{who}-topic")]
        );
        assert_eq!(
            strings(&local, "SELECT \"table\" FROM pragma_foreign_key_check").await?,
            Vec::<String>::new()
        );
    }
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn reports_all_missing_type_slugs_before_writing_either_destination(
    pool: PgPool,
) -> TestResult {
    let directory = tempfile::tempdir()?;
    let local = store(&directory, "local")?;
    let hub_rows = "INSERT INTO rh_topics (id, kind, slug, name, created_at, updated_at) VALUES
        (gen_random_uuid(), 'type', 'hobby', 'Hobby', now(), now()),
        (gen_random_uuid(), 'type', 'recipe', 'Recipe', now(), now());
        INSERT INTO rh_topics (id, kind, slug, name, parent_id, owner_user_id, created_at, updated_at)
        SELECT gen_random_uuid(), 'entity', slug || '-example', name, id, 'alice', now(), now() FROM rh_topics
        WHERE slug IN ('hobby', 'recipe')";
    sqlx::raw_sql(hub_rows).execute(&pool).await?;
    let error = copy(&pool, &local, "alice", Direction::ToSqlite)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("destination is missing global types: hobby, recipe"),
        "{error}"
    );
    assert_eq!(
        strings(&local, "SELECT slug FROM rh_topics WHERE kind = 'entity'").await?,
        Vec::<String>::new()
    );

    sqlx::query("DELETE FROM rh_topics WHERE slug IN ('hobby-example', 'recipe-example', 'recipe', 'hobby')")
        .execute(&pool).await?;
    local.with_conn(|conn| conn.execute_batch(
        "INSERT INTO rh_topics (id, kind, slug, name, created_at, updated_at) VALUES
        ('20000000-0000-0000-0000-000000000001', 'type', 'hobby', 'Hobby', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
        ('20000000-0000-0000-0000-000000000002', 'type', 'recipe', 'Recipe', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
        INSERT INTO rh_topics (id, kind, slug, name, parent_id, owner_user_id, created_at, updated_at)
        SELECT '30000000-0000-0000-0000-00000000000' || row_number() OVER (ORDER BY slug),
           'entity', slug || '-example', name, id, 'alice', created_at, updated_at
        FROM rh_topics WHERE slug IN ('hobby', 'recipe')",
    ).map_err(SqliteError::Pragma)).await?;
    let error = copy(&pool, &local, "alice", Direction::ToPostgres)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("destination is missing global types: hobby, recipe"),
        "{error}"
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rh_topics WHERE owner_user_id = 'alice'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(count, 0);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_unowned_hub_redirect_takes_the_owner_that_arrives(pool: PgPool) -> TestResult {
    sqlx::raw_sql(
        "INSERT INTO rh_wiki_topics (slug, title, owner_user_id) VALUES
            ('alice-page', 'A', 'alice'), ('bob-page', 'B', 'bob');
         INSERT INTO rh_wiki_redirects (from_slug, to_slug, created_at, owner_user_id)
            VALUES ('legacy', 'alice-page', now(), NULL), ('legacy-bob', 'bob-page', now(), NULL)",
    )
    .execute(&pool)
    .await?;
    let directory = tempfile::tempdir()?;
    let local = store(&directory, "local")?;
    copy(&pool, &local, "alice", Direction::ToSqlite).await?;
    copy(&pool, &local, "alice", Direction::ToPostgres).await?;
    let owners: Vec<String> = sqlx::query_scalar(
        "SELECT from_slug || ':' || coalesce(owner_user_id, '') FROM rh_wiki_redirects
         ORDER BY from_slug",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(owners, vec!["legacy:alice", "legacy-bob:"]);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn exports_redirects_by_creator_and_target(pool: PgPool) -> TestResult {
    sqlx::raw_sql(
        "INSERT INTO rh_wiki_topics (slug, title, owner_user_id) VALUES
            ('alice-page', 'A', 'alice'), ('bob-page', 'B', 'bob');
         SELECT set_config('rh.principal', 'alice', true);
         INSERT INTO rh_wiki_redirects (from_slug, to_slug, created_at) VALUES
            ('alice-redirect', 'alice-page', now()), ('alice-cross', 'bob-page', now());
         SELECT set_config('rh.principal', 'bob', true);
         INSERT INTO rh_wiki_redirects (from_slug, to_slug, created_at)
            VALUES ('bob-cross', 'alice-page', now());
         INSERT INTO rh_wiki_redirects (from_slug, to_slug, created_at, owner_user_id)
            VALUES ('legacy', 'alice-page', now(), NULL), ('legacy-bob', 'bob-page', now(), NULL)",
    )
    .execute(&pool)
    .await?;
    let directory = tempfile::tempdir()?;
    let local = store(&directory, "local")?;
    let report = copy(&pool, &local, "alice", Direction::ToSqlite).await?;
    assert_eq!(
        strings(
            &local,
            "SELECT from_slug || ':' || owner_user_id FROM ros_wiki_redirects ORDER BY from_slug"
        )
        .await?,
        vec!["alice-redirect:alice", "legacy:alice"]
    );
    assert_eq!(report.omissions.redirects_owned_by_others, 1);
    assert_eq!(
        copy(&pool, &local, "alice", Direction::ToPostgres)
            .await?
            .added
            .values()
            .sum::<u64>(),
        0
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn carries_used_global_tag_display_without_private_or_unused_vocabulary(
    pool: PgPool,
) -> TestResult {
    sqlx::raw_sql(
        "INSERT INTO rh_conversations (session_key, agent, owner_user_id) VALUES ('alice:tagged', 'codex', 'alice');
         INSERT INTO rh_tags (entity_type, entity_id, key, value, source, state, owner_user_id)
         SELECT 'conversation', c.id, 'topic', names.value, 'user', 'accepted', 'alice'
         FROM rh_conversations c CROSS JOIN (VALUES ('gardening'), ('foreign'), ('bare')) names(value)
         WHERE c.session_key = 'alice:tagged';
         INSERT INTO rh_tag_taxonomy (key, value, display, owner_user_id) VALUES
         ('topic', 'gardening', 'Garden planning', NULL),
         ('topic', 'secret', 'Unused', NULL),
         ('topic', 'foreign', 'Private label', 'bob')",
    ).execute(&pool).await?;
    let directory = tempfile::tempdir()?;
    let local = store(&directory, "local")?;
    copy(&pool, &local, "alice", Direction::ToSqlite).await?;
    assert_tag_vocabulary(&local).await?;
    local
        .with_conn(|conn| conn.execute(
            "INSERT INTO ros_tag_taxonomy (key, value, display, created_at, updated_at)
             VALUES ('topic', 'planned', 'Planned', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [],
        ).map_err(SqliteError::Pragma))
        .await?;
    copy(&pool, &local, "alice", Direction::ToPostgres).await?;
    let owner: Option<String> =
        sqlx::query_scalar("SELECT owner_user_id FROM rh_tag_taxonomy WHERE value = 'planned'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(owner.as_deref(), Some("alice"));
    Ok(())
}

async fn assert_tag_vocabulary(local: &SqliteStore) -> TestResult {
    use kernel::memory::{Limit, MemoryStore as _, TagListQuery, TagTaxonomyQuery};

    let limit = Limit::clamped(None, 10, 1..=10);
    let vocabulary = local
        .list_tag_taxonomy(TagTaxonomyQuery {
            key: Some("topic".to_owned()),
            state: None,
            limit,
        })
        .await?;
    let labels: Vec<_> = vocabulary
        .into_iter()
        .map(|entry| (entry.value, entry.display))
        .collect();
    assert_eq!(
        labels,
        vec![("gardening".to_owned(), "Garden planning".to_owned())]
    );
    let tags = local
        .list_tags(TagListQuery {
            entity_type: None,
            entity_id: None,
            key: Some("topic".to_owned()),
            value: None,
            states: Vec::new(),
            limit,
        })
        .await?;
    let mut values: Vec<_> = tags
        .into_iter()
        .map(|tag| (tag.value, tag.display))
        .collect();
    values.sort_unstable();
    assert_eq!(
        values,
        vec![
            ("bare".to_owned(), String::new()),
            ("foreign".to_owned(), String::new()),
            ("gardening".to_owned(), String::new()),
        ]
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn legacy_deletes_stay_unattributed_and_redeletes_gain_the_live_topic_owner(
    pool: PgPool,
) -> TestResult {
    sqlx::raw_sql(
        "INSERT INTO rh_topic_tombstones (kind, slug, deleted_at) VALUES
          ('work', 'legacy-work', now()), ('entity', 'legacy-entity', now()),
          ('type', 'legacy-type', now()), ('work', 'again', now());
         INSERT INTO rh_topics (id, kind, slug, name, owner_user_id, created_at, updated_at)
         VALUES ('00000000-0000-0000-0000-000000000011', 'work', 'again', 'Again', 'alice', now(), now())",
    )
    .execute(&pool)
    .await?;
    let directory = tempfile::tempdir()?;
    let local = store(&directory, "local")?;
    let again = TopicId::from_uuid(uuid::uuid!("00000000-0000-0000-0000-000000000011"));
    PostgresMemory::new(pool.clone()).delete(again).await?;
    let report = copy(&pool, &local, "alice", Direction::ToSqlite).await?;
    assert_eq!(report.omissions.unattributed_topic_tombstones, 3);
    assert_eq!(
        strings(
            &local,
            "SELECT slug || ':' || owner_user_id FROM rh_topic_tombstones"
        )
        .await?,
        vec!["again:alice"]
    );
    let remaining: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT kind FROM rh_topic_tombstones WHERE owner_user_id IS NULL ORDER BY kind",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(remaining, vec!["entity", "type", "work"]);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn comparison_reads_null_and_transfer_assigns_only_local_personal_rows(
    pool: PgPool,
) -> TestResult {
    let directory = tempfile::tempdir()?;
    let local = store(&directory, "local")?;
    let restored = store(&directory, "restored")?;
    local
        .with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO rh_topics (id, kind, slug, name, created_at, updated_at)
         VALUES ('00000000-0000-0000-0000-000000000011', 'work', 'unowned', 'An old local topic',
           '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
         INSERT INTO rh_topic_tombstones (kind, slug, deleted_at)
         VALUES ('work', 'unknown', '2026-01-01T00:00:00Z')",
            )
            .map_err(SqliteError::Pragma)
        })
        .await?;
    let before = verify::sqlite(&pool, &local).await?;
    let report = copy(&pool, &local, "alice", Direction::ToPostgres).await?;
    assert_eq!(report.omissions.unattributed_topic_tombstones, 1);
    copy(&pool, &restored, "alice", Direction::ToSqlite).await?;
    let after = verify::sqlite(&pool, &restored).await?;
    assert_ne!(before.get("rh_topics"), after.get("rh_topics"));
    assert_eq!(
        strings(
            &restored,
            "SELECT owner_user_id FROM rh_topics WHERE slug = 'unowned'"
        )
        .await?,
        vec!["alice"]
    );
    assert_eq!(
        strings(&restored, "SELECT slug FROM rh_topic_tombstones").await?,
        Vec::<String>::new()
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn new_ownerless_local_deletes_can_be_attributed_to_the_sqlite_principal(
    pool: PgPool,
) -> TestResult {
    let directory = tempfile::tempdir()?;
    let local = store(&directory, "local")?;
    local
        .with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO rh_topic_tombstones (kind, slug, deleted_at)
         VALUES ('work', 'again', '2026-01-01T00:00:00Z');
         INSERT INTO rh_topics (id, kind, slug, name, created_at, updated_at)
         VALUES ('00000000-0000-0000-0000-000000000011', 'work', 'again', 'Again',
                 '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            )
            .map_err(SqliteError::Pragma)
        })
        .await?;
    let again = TopicId::from_uuid(uuid::uuid!("00000000-0000-0000-0000-000000000011"));
    local.delete(again).await?;
    copy(&pool, &local, "alice", Direction::ToPostgres).await?;
    let owner: Option<String> =
        sqlx::query_scalar("SELECT owner_user_id FROM rh_topic_tombstones WHERE slug = 'again'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(owner.as_deref(), Some("alice"));
    assert_eq!(
        copy(&pool, &local, "alice", Direction::ToSqlite)
            .await?
            .added
            .get("rh_topic_tombstones"),
        Some(&0)
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn sqlite_page_writers_preserve_transferred_fields_on_return_to_postgres(
    pool: PgPool,
) -> TestResult {
    let directory = tempfile::tempdir()?;
    let seed = store(&directory, "seed")?;
    let local = store(&directory, "local")?;
    super::fixture(&seed).await?;
    super::authorization(&pool).await?;
    copy(&pool, &seed, "alice", Direction::ToPostgres).await?;
    sqlx::raw_sql(
        r"INSERT INTO rh_wiki_topics (slug, title, topic_id, owner_user_id, entities, related)
             VALUES ('quill', 'Quill', '00000000-0000-0000-0000-000000000013', 'alice',
                     ARRAY['Editor'], ARRAY['transfer']),
                    ('null-archive', 'Null archive', '00000000-0000-0000-0000-000000000011', 'alice',
                     '{}', '{}');
           UPDATE rh_wiki_topics SET entities = ARRAY['Editor'], related = ARRAY['quill']
           WHERE slug = 'transfer'",
    ).execute(&pool).await?;
    let before = carried_page_fields(&pool).await?;
    copy(&pool, &local, "alice", Direction::ToSqlite).await?;
    run_local_page_writers(&local).await?;
    sqlx::raw_sql(
        "DELETE FROM rh_wiki_redirects WHERE to_slug IN ('quill', 'null-archive', 'transfer');
         DELETE FROM rh_wiki_topics WHERE slug IN ('quill', 'null-archive', 'transfer')",
    )
    .execute(&pool)
    .await?;
    copy(&pool, &local, "alice", Direction::ToPostgres).await?;
    assert_eq!(carried_page_fields(&pool).await?, before);
    Ok(())
}

async fn carried_page_fields(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT jsonb_build_array(w.slug, w.topic_id, w.visibility, w.scope, w.local_slug,
                w.entities, w.related, w.archived_sections, s.owner_user_id, s.directory,
                (SELECT system_prompt FROM rh_agent_presets WHERE id = 'one'))::text
         FROM rh_wiki_topics w LEFT JOIN rh_wiki_scopes s ON w.scope = s.scope
         WHERE w.slug IN ('quill', 'null-archive', 'transfer') ORDER BY w.slug",
    )
    .fetch_all(pool)
    .await
}

async fn run_local_page_writers(local: &SqliteStore) -> TestResult {
    use kernel::topics::TopicPageStore as _;
    use kernel::wiki::builder::{ExtractWiki, WikiBuilderStore as _, WikiRouting};
    use kernel::wiki::page::{WikiAction, WikiPatch, WikiSlug};

    let now = "2026-01-03T00:00:00Z".parse()?;
    let changed = local
        .apply_wiki(
            &ExtractWiki {
                summary_id: uuid::uuid!("00000000-0000-0000-0000-000000000007"),
                conversation_id: Some(uuid::uuid!("00000000-0000-0000-0000-000000000002")),
            },
            vec![WikiPatch {
                action: WikiAction::Update,
                slug: WikiSlug::try_from("transfer".to_owned())?,
                title: "Updated by the wiki builder".to_owned(),
                aliases: Vec::new(),
                tags: Vec::new(),
                entities: vec!["Editor".to_owned()],
                summary: "Revised summary".to_owned(),
                article: None,
                related: None,
                history_entry: None,
                entity_type: None,
                facts: None,
            }],
            &WikiRouting::default(),
            now,
        )
        .await?;
    assert_eq!(changed, vec!["transfer"]);
    for (slug, topic) in [
        (
            "null-archive",
            uuid::uuid!("00000000-0000-0000-0000-000000000011"),
        ),
        (
            "transfer",
            uuid::uuid!("00000000-0000-0000-0000-000000000012"),
        ),
    ] {
        let mut page = local
            .topic_page(&WikiSlug::try_from(slug.to_owned())?)
            .await?
            .ok_or("missing copied page")?;
        page.title = format!("Saved locally {slug}");
        local
            .save_topic_page(TopicId::from_uuid(topic), &page, now)
            .await?;
    }
    publish_local_entity(local, now).await
}

async fn publish_local_entity(local: &SqliteStore, now: jiff::Timestamp) -> TestResult {
    use kernel::wiki::entity_pass::{EntityProse, EntityStore as _, EntityView, PublishOutcome};

    let view = EntityView {
        id: uuid::uuid!("00000000-0000-0000-0000-000000000013"),
        name: "Quill".to_owned(),
        slug: "quill".to_owned(),
        type_name: String::new(),
        type_slug: String::new(),
        aliases: Vec::new(),
        facts: Vec::new(),
        history: Vec::new(),
        snapshot: String::new(),
        owner: Some("alice".to_owned()),
    };
    let prose = EntityProse {
        summary: "Republished locally".to_owned(),
        article: "Edited entity article".to_owned(),
    };
    let fresh = match local.publish(&view, &prose, now).await? {
        PublishOutcome::Stale(fresh) => fresh,
        PublishOutcome::Stored { .. } => return Err("expected an updated entity snapshot".into()),
    };
    let result = local.publish(&fresh, &prose, now).await?;
    assert!(matches!(result, PublishOutcome::Stored { slug, .. } if slug == "quill"));
    Ok(())
}
