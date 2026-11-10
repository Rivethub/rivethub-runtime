use std::collections::BTreeSet;
use std::sync::Arc;

use kernel::memory::BrowseFilter;
use kernel::memory::Limit;
use kernel::memory::MemoryBrowseOrder;
use kernel::memory::MemoryScope;
use kernel::memory::MemoryState;
use kernel::memory::MemoryStore;
use kernel::memory::SearchMode;
use kernel::memory::SearchQuery;
use kernel::memory::SessionCatalog;
use kernel::memory::SessionDeletion as _;
use kernel::memory::SessionQuery;
use kernel::memory::SessionView;
use kernel::model::ModelProvider as _;
use kernel::topics::Placement;
use kernel::topics::PlacementSource;
use kernel::topics::TopicStore;
use memory_postgres::PostgresMemory;
use memory_postgres::QueryEmbed;
use pgvector::HalfVector;
use sqlx::PgPool;
use testkit::FakeProvider;
use testkit::session_catalog;
use testkit::topics::at;
use testkit::topics::new_topic;
use uuid::Uuid;
use wire::session_id::SessionId;
use wire::session_id::SessionKey;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn lists_sessions_newest_first_with_counts_and_filters(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool);
    session_catalog::lists_sessions_newest_first_with_counts_and_filters(&memory, &memory).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn keep_and_topics_shape_the_views(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool);
    session_catalog::keep_and_topics_shape_the_views(&memory, &memory, &memory, &memory).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn trash_is_reversible_and_leaves_recall(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool);
    session_catalog::trash_is_reversible_and_leaves_recall(&memory, &memory, &memory).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn purge_removes_only_old_trash_through_deletion(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool);
    session_catalog::purge_removes_only_old_trash_through_deletion(&memory, &memory, &memory).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn excludes_children_and_derives_titles(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool);
    session_catalog::excludes_children_and_derives_titles(&memory, &memory).await
}

async fn conversation(pool: &PgPool, session: &SessionId) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar("SELECT id FROM rh_conversations WHERE session_key = $1")
        .bind(session.as_str())
        .fetch_one(pool)
        .await
}

async fn summarize(
    pool: &PgPool,
    session: &SessionId,
    depth: i32,
    content: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO rh_summaries (conversation_id, depth, kind, content, owner_user_id)
         VALUES ($1, $2, CASE WHEN $2 = 0 THEN 'leaf' ELSE 'branch' END, $3, 'owner')",
    )
    .bind(conversation(pool, session).await?)
    .bind(depth)
    .bind(content)
    .execute(pool)
    .await?;
    Ok(())
}

async fn flag(pool: &PgPool, key: &str, payload: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO rh_flagged_jobs (task, job_key, payload, priority, max_attempts, failures,
                                      last_error, first_failed_at, last_failed_at, flagged_at)
         VALUES ('embed-target', $1, $2::jsonb, 0, 5, 5, 'boom', now(), now(), now())",
    )
    .bind(key)
    .bind(payload)
    .execute(pool)
    .await?;
    Ok(())
}

async fn state(memory: &PostgresMemory, session: &SessionId) -> Result<MemoryState, String> {
    memory
        .get(&SessionKey::from(session))
        .await
        .map_err(|error| error.to_string())?
        .map(|record| record.memory)
        .ok_or_else(|| format!("{session} missing"))
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn memory_state_follows_summaries_and_open_flagged_jobs(pool: PgPool) -> TestResult {
    crate::common::install_graphile(&pool).await?;
    let memory = PostgresMemory::new(pool.clone());
    let seeded = session_catalog::seed(&memory).await?;
    assert_eq!(state(&memory, &seeded.alpha).await?, MemoryState::Queued);

    let delta = conversation(&pool, &seeded.delta).await?;
    sqlx::query("SELECT graphile_worker.add_job('compact-conversation', json_build_object('conversationId', $1::text))")
        .bind(delta)
        .execute(&pool)
        .await?;
    assert_eq!(
        state(&memory, &seeded.delta).await?,
        MemoryState::Queued,
        "a pending compaction job is still queued"
    );

    summarize(&pool, &seeded.alpha, 0, "leaf notes about alpha").await?;
    summarize(&pool, &seeded.alpha, 1, "branch   overview\nof alpha").await?;
    let alpha = memory
        .get(&SessionKey::from(&seeded.alpha))
        .await?
        .ok_or("alpha missing")?;
    assert_eq!(alpha.memory, MemoryState::Summarized);
    assert_eq!(alpha.gist.as_deref(), Some("branch overview of alpha"));

    let gamma = conversation(&pool, &seeded.gamma).await?;
    flag(
        &pool,
        "gamma-conversation",
        &format!(r#"{{"conversationId":"{gamma}"}}"#),
    )
    .await?;
    assert_eq!(state(&memory, &seeded.gamma).await?, MemoryState::Flagged);

    let message: Uuid =
        sqlx::query_scalar("SELECT id FROM rh_messages WHERE conversation_id = $1 LIMIT 1")
            .bind(delta)
            .fetch_one(&pool)
            .await?;
    flag(
        &pool,
        "delta-message",
        &format!(r#"{{"targetTable":"ros_messages","targetId":"{message}"}}"#),
    )
    .await?;
    assert_eq!(state(&memory, &seeded.delta).await?, MemoryState::Flagged);
    sqlx::query("UPDATE rh_flagged_jobs SET dismissed_at = now() WHERE job_key = 'delta-message'")
        .execute(&pool)
        .await?;
    assert_eq!(
        state(&memory, &seeded.delta).await?,
        MemoryState::Queued,
        "a dismissed flag no longer counts"
    );

    flag(
        &pool,
        "beta-flag",
        &format!(
            r#"{{"conversationId":"{}"}}"#,
            conversation(&pool, &seeded.beta).await?
        ),
    )
    .await?;
    assert_eq!(
        state(&memory, &seeded.beta).await?,
        MemoryState::Flagged,
        "flagged beats capturing"
    );
    Ok(())
}

fn search(text: &str, mode: SearchMode, scope: MemoryScope) -> SearchQuery {
    SearchQuery {
        text: text.to_owned(),
        mode,
        scope,
        agent: None,
        role: None,
        exclude_roles: Vec::new(),
        tool_name: None,
        tag: None,
        limit: Limit::clamped(Some(50), 50, 1..=50),
        since: None,
        before: None,
        window: None,
        expand: None,
        synthesize: None,
    }
}

async fn recalled(memory: &PostgresMemory, query: SearchQuery) -> Result<BTreeSet<String>, String> {
    Ok(MemoryStore::search(memory, query)
        .await
        .map_err(|error| error.to_string())?
        .results
        .into_iter()
        .filter_map(|hit| hit.session_id)
        .collect())
}

fn browse() -> BrowseFilter {
    BrowseFilter {
        conversation_id: None,
        agent: None,
        role: None,
        tool_name: None,
        tag: None,
        window: None,
        since: None,
        before: None,
        include_tools: false,
        order: MemoryBrowseOrder::Desc,
        limit: Limit::clamped(Some(200), 200, 1..=200),
    }
}

async fn embed_everything(pool: &PgPool, provider: &FakeProvider) -> TestResult {
    let vector = provider
        .embed(vec!["quokka".to_owned()])
        .await?
        .pop()
        .ok_or("the fake returned no embedding")?
        .vector;
    let vector = HalfVector::from_f32_slice(&vector);
    sqlx::query("UPDATE rh_messages SET embedding = $1")
        .bind(&vector)
        .execute(pool)
        .await?;
    sqlx::query("UPDATE rh_summaries SET embedding = $1")
        .bind(&vector)
        .execute(pool)
        .await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn trashed_sessions_leave_every_search_mode_and_browse(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool.clone()).with_query_embed(QueryEmbed::new(
        Arc::new(FakeProvider::fnv_embeddings(1024)),
        None,
        None,
        None,
    ));
    let seeded = session_catalog::seed(&memory).await?;
    for session in [&seeded.alpha, &seeded.beta, &seeded.gamma, &seeded.delta] {
        summarize(&pool, session, 0, "quokka summary").await?;
    }
    embed_everything(&pool, &FakeProvider::fnv_embeddings(1024)).await?;
    memory
        .trash_session(seeded.delta.as_str(), Some("user-1"), at(70))
        .await?;
    let delta = seeded.delta.to_string();
    for mode in [
        SearchMode::Fts,
        SearchMode::Trigram,
        SearchMode::Regex,
        SearchMode::Vector,
        SearchMode::Hybrid,
    ] {
        for scope in [MemoryScope::Messages, MemoryScope::Summaries] {
            let found = recalled(&memory, search("quokka", mode, scope)).await?;
            assert_eq!(
                found.len(),
                3,
                "{mode:?} {scope:?} keeps the others: {found:?}"
            );
            assert!(
                !found.contains(&delta),
                "{mode:?} {scope:?} leaves the trash"
            );
        }
    }
    let browsed: BTreeSet<String> = memory
        .browse(browse())
        .await?
        .into_iter()
        .map(|entry| entry.session_id)
        .collect();
    assert_eq!(browsed.len(), 3, "browse keeps the others: {browsed:?}");
    assert!(!browsed.contains(&delta), "browse leaves the trash");

    memory.restore_session(seeded.delta.as_str()).await?;
    let found = recalled(
        &memory,
        search("quokka", SearchMode::Vector, MemoryScope::Both),
    )
    .await?;
    assert!(
        found.contains(&delta),
        "restored sessions are recalled again"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn trashed_sessions_leave_the_topic_views(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool);
    let seeded = session_catalog::seed(&memory).await?;
    let topic = memory.create(new_topic("Release", &[], None)?).await?;
    for session in [&seeded.alpha, &seeded.beta] {
        memory
            .place(Placement {
                session: session.clone(),
                topic: topic.id,
                by: PlacementSource::You,
                at: at(80),
                owner: testkit::owner().ok(),
            })
            .await?;
    }
    let count = async || -> Result<u64, Box<dyn std::error::Error>> {
        Ok(TopicStore::list(&memory)
            .await?
            .into_iter()
            .find(|listing| listing.topic.id == topic.id)
            .ok_or("topic missing")?
            .session_count)
    };
    let placed = async || -> Result<Vec<String>, Box<dyn std::error::Error>> {
        Ok(memory
            .sessions(topic.id)
            .await?
            .into_iter()
            .map(|placed| placed.placement.session.to_string())
            .collect())
    };
    assert_eq!(count().await?, 2);
    memory
        .trash_session(seeded.alpha.as_str(), None, at(81))
        .await?;
    assert_eq!(count().await?, 1, "the trashed session leaves the count");
    assert_eq!(placed().await?, vec![seeded.beta.to_string()]);
    memory.restore_session(seeded.alpha.as_str()).await?;
    assert_eq!(count().await?, 2, "restore brings it back");
    assert_eq!(placed().await?.len(), 2);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn exclude_applies_to_every_view(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool);
    let seeded = session_catalog::seed(&memory).await?;
    memory
        .trash_session(seeded.gamma.as_str(), None, at(90))
        .await?;
    let mut trash = SessionQuery::new(SessionView::Trash);
    assert_eq!(SessionCatalog::list(&memory, &trash).await?.items.len(), 1);
    trash.exclude = vec![seeded.gamma.to_string()];
    assert_eq!(SessionCatalog::list(&memory, &trash).await?.items.len(), 0);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn deleting_a_trashed_session_clears_its_trash_mark(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    let seeded = session_catalog::seed(&memory).await?;
    memory
        .trash_session(seeded.beta.as_str(), Some("user-1"), at(95))
        .await?;
    memory
        .delete_session(seeded.beta.as_str(), Some("user-1"), at(96))
        .await?;
    let marks = memory.trashed(&[seeded.beta.to_string()]).await?;
    assert!(marks.is_empty(), "{marks:?}");
    let left = SessionCatalog::list(&memory, &SessionQuery::new(SessionView::Trash)).await?;
    assert_eq!(left.items.len(), 0);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn purge_tombstones_with_the_trash_mark(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    let seeded = session_catalog::seed(&memory).await?;
    memory
        .trash_session(seeded.alpha.as_str(), Some("user-1"), at(100))
        .await?;
    memory.purge_trashed(at(100), 10).await?;
    let (deleted_at, deleted_by): (chrono::DateTime<chrono::Utc>, Option<String>) = sqlx::query_as(
        "SELECT deleted_at, deleted_by FROM rh_session_tombstones WHERE session_key = $1",
    )
    .bind(seeded.alpha.as_str())
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        (deleted_at, deleted_by.as_deref()),
        (at(100), Some("user-1"))
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn lists_bare_claude_sessions_under_their_harness(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool);
    session_catalog::lists_bare_claude_sessions_under_their_harness(&memory, &memory).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn records_answer_bare_and_keyed_sessions_in_one_lookup(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool);
    testkit::session_records::records_answer_bare_and_keyed_sessions_in_one_lookup(&memory, &memory)
        .await
}
