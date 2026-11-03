use kernel::memory::DeletedSession;
use kernel::memory::SessionDeletion;
use kernel::topics::Placement;
use kernel::topics::PlacementSource;
use kernel::topics::TopicStore;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use testkit::session_delete;
use testkit::topics::at;
use testkit::topics::new_topic;
use uuid::Uuid;
use wire::session_id::SessionId;

type TestResult = Result<(), Box<dyn std::error::Error>>;

async fn conversations(pool: &PgPool, session: &SessionId) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar("SELECT id FROM rh_conversations WHERE session_key = $1 ORDER BY agent")
        .bind(session.as_str())
        .fetch_all(pool)
        .await
}

async fn seed_related(pool: &PgPool, session: &SessionId) -> Result<Vec<Uuid>, sqlx::Error> {
    let mut summaries = Vec::new();
    for conversation in conversations(pool, session).await? {
        let summary = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO rh_summaries (id, conversation_id, content, owner_user_id) VALUES ($1, $2, 'summary text', 'owner')",
        )
        .bind(summary)
        .bind(conversation)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO rh_summary_sources (summary_id, message_id, ordinal)
             SELECT $1, id, 0 FROM rh_messages WHERE conversation_id = $2",
        )
        .bind(summary)
        .bind(conversation)
        .execute(pool)
        .await?;
        for (entity_type, entity) in [("conversation", conversation), ("summary", summary)] {
            sqlx::query(
                "INSERT INTO rh_tags (entity_type, entity_id, key, value, source, state)
                 VALUES ($1, $2, 'project', 'demo', 'user', 'accepted')",
            )
            .bind(entity_type)
            .bind(entity)
            .execute(pool)
            .await?;
        }
        summaries.push(summary);
    }
    sqlx::query(
        "INSERT INTO rh_session_startup (session_id, topic_id, tokens, sections, created_at)
         VALUES ($1, $2, 0, '[]'::jsonb, now())",
    )
    .bind(session.as_str())
    .bind(Uuid::new_v4())
    .execute(pool)
    .await?;
    sqlx::query("INSERT INTO rh_session_history (session_id, marked_at) VALUES ($1, now())")
        .bind(session.as_str())
        .execute(pool)
        .await?;
    Ok(summaries)
}

async fn count(pool: &PgPool, sql: &str, session: &SessionId) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .bind(session.as_str())
        .fetch_one(pool)
        .await
}

async fn remaining(pool: &PgPool, session: &SessionId) -> Result<[i64; 8], sqlx::Error> {
    let of_conversations = "SELECT id FROM rh_conversations WHERE session_key = $1";
    let of_summaries =
        format!("SELECT id FROM rh_summaries WHERE conversation_id IN ({of_conversations})");
    Ok([
        count(
            pool,
            "SELECT count(*) FROM rh_conversations WHERE session_key = $1",
            session,
        )
        .await?,
        count(
            pool,
            &format!(
                "SELECT count(*) FROM rh_messages WHERE conversation_id IN ({of_conversations})"
            ),
            session,
        )
        .await?,
        count(
            pool,
            &format!("SELECT count(*) FROM ({of_summaries}) s"),
            session,
        )
        .await?,
        count(
            pool,
            &format!(
                "SELECT count(*) FROM rh_summary_sources WHERE summary_id IN ({of_summaries})"
            ),
            session,
        )
        .await?,
        count(
            pool,
            &format!(
                "SELECT count(*) FROM rh_tags
                  WHERE entity_id IN ({of_conversations}) OR entity_id IN ({of_summaries})"
            ),
            session,
        )
        .await?,
        count(
            pool,
            "SELECT count(*) FROM rh_session_placements WHERE session_id = $1",
            session,
        )
        .await?,
        count(
            pool,
            "SELECT count(*) FROM rh_session_startup WHERE session_id = $1",
            session,
        )
        .await?,
        count(
            pool,
            "SELECT count(*) FROM rh_session_history WHERE session_id = $1",
            session,
        )
        .await?,
    ])
}

async fn place(memory: &PostgresMemory, sessions: &[&SessionId]) -> TestResult {
    let topic = memory.create(new_topic("Release", &[], None)?).await?;
    for session in sessions {
        memory
            .place(Placement {
                session: (*session).clone(),
                topic: topic.id,
                by: PlacementSource::You,
                at: at(3),
                owner: testkit::owner().ok(),
            })
            .await?;
    }
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn delete_removes_one_session_and_leaves_its_sibling(pool: PgPool) -> TestResult {
    crate::common::install_graphile(&pool).await?;
    let memory = PostgresMemory::new(pool.clone());
    let (deleted, sibling) = session_delete::seed(&memory).await?;
    place(&memory, &[&deleted, &sibling]).await?;
    seed_related(&pool, &deleted).await?;
    seed_related(&pool, &sibling).await?;
    let sibling_before = remaining(&pool, &sibling).await?;
    assert_eq!(remaining(&pool, &deleted).await?, [2, 4, 2, 4, 4, 1, 1, 1]);
    let erased = memory
        .delete_session(deleted.as_str(), Some("user-1"), at(10))
        .await?;
    assert_eq!(
        erased,
        DeletedSession {
            conversations: 2,
            messages: 4,
            summaries: 2,
        }
    );
    assert_eq!(remaining(&pool, &deleted).await?, [0; 8]);
    assert_eq!(remaining(&pool, &sibling).await?, sibling_before);
    let deleted_by: Option<String> =
        sqlx::query_scalar("SELECT deleted_by FROM rh_session_tombstones WHERE session_key = $1")
            .bind(deleted.as_str())
            .fetch_one(&pool)
            .await?;
    assert_eq!(deleted_by.as_deref(), Some("user-1"));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn capture_and_append_for_a_deleted_session_write_nothing(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool);
    session_delete::capture_for_a_deleted_session_writes_nothing(&memory, &memory).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_empty_session_is_reported_until_it_is_deleted(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool);
    session_delete::an_empty_session_is_reported_until_it_is_deleted(&memory, &memory).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn activity_counts_turns_tools_and_placements(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool);
    session_delete::activity_counts_turns_tools_and_placements(&memory, &memory, &memory).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_summary_cited_by_a_wiki_page_marks_the_session_cited(pool: PgPool) -> TestResult {
    crate::common::install_graphile(&pool).await?;
    let memory = PostgresMemory::new(pool.clone());
    let (deleted, sibling) = session_delete::seed(&memory).await?;
    let summaries = seed_related(&pool, &sibling).await?;
    sqlx::query("INSERT INTO rh_wiki_topics (slug, title) VALUES ('release', 'Release')")
        .execute(&pool)
        .await?;
    sqlx::query("INSERT INTO rh_wiki_citations (topic_slug, summary_id, owner_user_id) VALUES ('release', $1, 'owner')")
        .bind(summaries.first().copied().ok_or("a seeded summary")?)
        .execute(&pool)
        .await?;
    let activity = memory
        .session_activity(&[deleted.as_str().to_owned(), sibling.as_str().to_owned()])
        .await?;
    assert_eq!(
        activity.get(sibling.as_str()).map(|entry| entry.cited),
        Some(true)
    );
    assert_eq!(
        activity.get(deleted.as_str()).map(|entry| entry.cited),
        Some(false)
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn the_sweep_removes_rows_written_behind_the_gateway(pool: PgPool) -> TestResult {
    crate::common::install_graphile(&pool).await?;
    let memory = PostgresMemory::new(pool.clone());
    let (deleted, sibling) = session_delete::seed(&memory).await?;
    memory
        .delete_session(deleted.as_str(), None, at(10))
        .await?;
    let conversation: Uuid = sqlx::query_scalar(
        "INSERT INTO rh_conversations (session_key, agent, owner_user_id) VALUES ($1, 'external', 'owner') RETURNING id",
    )
    .bind(deleted.as_str())
    .fetch_one(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO rh_messages (conversation_id, agent, channel, role, content, created_at, owner_user_id)
         VALUES ($1, 'external', 'cli', 'user', 'written directly', $2, 'owner')",
    )
    .bind(conversation)
    .bind(at(4))
    .execute(&pool)
    .await?;
    let topic = memory.create(new_topic("Late", &[], None)?).await?;
    memory
        .place(Placement {
            session: deleted.clone(),
            topic: topic.id,
            by: PlacementSource::Categorizer,
            at: at(11),
            owner: testkit::owner().ok(),
        })
        .await?;
    let purged = memory.purge_tombstoned(100).await?;
    assert_eq!(
        purged,
        DeletedSession {
            conversations: 1,
            messages: 1,
            summaries: 0,
        }
    );
    assert_eq!(
        memory.purge_tombstoned(100).await?,
        DeletedSession::default()
    );
    assert_eq!(remaining(&pool, &deleted).await?, [0; 8]);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM rh_conversations WHERE session_key = $1",
            &sibling
        )
        .await?,
        1
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn the_sweep_lifts_the_tombstone_when_the_session_was_used_again(pool: PgPool) -> TestResult {
    crate::common::install_graphile(&pool).await?;
    let memory = PostgresMemory::new(pool.clone());
    let (deleted, _sibling) = session_delete::seed(&memory).await?;
    memory
        .delete_session(deleted.as_str(), None, at(10))
        .await?;
    let conversation: Uuid = sqlx::query_scalar(
        "INSERT INTO rh_conversations (session_key, agent, owner_user_id) VALUES ($1, 'external', 'owner') RETURNING id",
    )
    .bind(deleted.as_str())
    .fetch_one(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO rh_messages (conversation_id, agent, channel, role, content, owner_user_id)
         VALUES ($1, 'external', 'cli', 'user', 'new work written directly', 'owner')",
    )
    .bind(conversation)
    .execute(&pool)
    .await?;
    assert_eq!(
        memory.purge_tombstoned(100).await?,
        DeletedSession::default()
    );
    assert!(
        memory
            .tombstones(&[deleted.as_str().to_owned()])
            .await?
            .is_empty()
    );
    assert_eq!(remaining(&pool, &deleted).await?.get(1), Some(&1));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_deleted_session_used_again_comes_back(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool);
    session_delete::a_deleted_session_used_again_comes_back(&memory, &memory).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_kept_session_is_remembered_from_its_latest_keep(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool);
    session_delete::a_kept_session_is_remembered_from_its_latest_keep(&memory).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_long_first_reply_is_read_as_its_opening_and_is_no_one_word_reply(
    pool: PgPool,
) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    let long = format!("{}é{}", "x".repeat(511), " and more");
    let mut tx = testkit::capture_tx(&pool).await?;
    sqlx::query(
        "INSERT INTO rh_conversations (id, session_key, agent, channel, owner_user_id) VALUES
           ('00000000-0000-4000-8000-0000000000c1', 'cli:long-reply', 'acme', 'cli', 'owner'),
           ('00000000-0000-4000-8000-0000000000c2', 'cli:short-reply', 'acme', 'cli', 'owner')",
    )
    .execute(&mut *tx)
    .await?;
    for (id, conversation, role, content) in [
        ("c3", "c1", "user", "hello"),
        ("c4", "c1", "assistant", long.as_str()),
        ("c5", "c2", "user", "hello"),
        ("c6", "c2", "assistant", "Done."),
    ] {
        sqlx::query(
            "INSERT INTO rh_messages (id, conversation_id, agent, channel, role, content, owner_user_id, created_at)
             VALUES ($1::uuid, $2::uuid, 'acme', 'cli', $3, $4, 'owner', now() + make_interval(secs => $5))",
        )
        .bind(format!("00000000-0000-4000-8000-0000000000{id}"))
        .bind(format!("00000000-0000-4000-8000-0000000000{conversation}"))
        .bind(role)
        .bind(content)
        .bind(f64::from(u8::from(role == "assistant")))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    let activity = memory
        .session_activity(&["cli:long-reply".to_owned(), "cli:short-reply".to_owned()])
        .await?;
    let reply = |key: &str| activity.get(key).and_then(|entry| entry.reply.clone());
    assert_eq!(
        reply("cli:long-reply"),
        Some(format!("{} …", "x".repeat(511)))
    );
    assert_eq!(reply("cli:short-reply"), Some("Done.".to_owned()));
    Ok(())
}
