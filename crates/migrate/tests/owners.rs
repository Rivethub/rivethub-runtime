mod owner;

use migrate::POSTGRES;
use sqlx::PgPool;

type TestResult = Result<(), Box<dyn std::error::Error>>;

async fn before_owners(pool: &PgPool) -> TestResult {
    for migration in POSTGRES
        .migrations
        .iter()
        .filter(|migration| migration.version < 42)
    {
        let sql: &'static str = Box::leak(migration.sql.as_str().to_string().into_boxed_str());
        sqlx::raw_sql(sql).execute(pool).await?;
    }
    Ok(())
}

async fn owners(pool: &PgPool, table: &str) -> Result<Vec<Option<String>>, sqlx::Error> {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT DISTINCT owner_user_id FROM {table} ORDER BY 1"
    )))
    .fetch_all(pool)
    .await
}

#[sqlx::test(migrations = false)]
async fn every_row_takes_the_owner_of_its_store(pool: PgPool) -> TestResult {
    before_owners(&pool).await?;
    sqlx::raw_sql(
        "SET rivet.defer_embed_enqueue = 'on';
         INSERT INTO rh_conversations (id, session_key, agent, channel, owner_user_id) VALUES
           ('00000000-0000-4000-8000-0000000000a1', 'cli:owned', 'rivet', 'cli', 'alice'),
           ('00000000-0000-4000-8000-0000000000a2', 'cli:unowned', 'rivet', 'cli', NULL);
         INSERT INTO rh_messages (conversation_id, agent, channel, role, content, owner_user_id) VALUES
           ('00000000-0000-4000-8000-0000000000a1', 'rivet', 'cli', 'user', 'one', 'alice'),
           ('00000000-0000-4000-8000-0000000000a2', 'rivet', 'cli', 'user', 'two', NULL);
         INSERT INTO rh_summaries (id, conversation_id, depth, content, kind, message_count)
           VALUES ('00000000-0000-4000-8000-0000000000b1', '00000000-0000-4000-8000-0000000000a2',
                   0, 'a leaf', 'leaf', 1);
         INSERT INTO rh_topics (id, kind, slug, name, created_at, updated_at)
           VALUES ('00000000-0000-4000-8000-0000000000c1', 'work', 'desk', 'Desk', now(), now());
         INSERT INTO rh_session_placements (session_id, topic_id, placed_by, placed_at)
           VALUES ('cli:unowned', '00000000-0000-4000-8000-0000000000c1', 'you', now());",
    )
    .execute(&pool)
    .await?;

    POSTGRES.run(&pool).await?;

    let alice = vec![Some("alice".to_owned())];
    for table in [
        "rh_conversations",
        "rh_messages",
        "rh_summaries",
        "rh_session_placements",
    ] {
        assert_eq!(owners(&pool, table).await?, alice, "{table}");
    }
    let topics: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT DISTINCT kind, owner_user_id FROM rh_topics ORDER BY 1")
            .fetch_all(&pool)
            .await?;
    assert_eq!(
        topics,
        vec![
            ("type".to_owned(), None),
            ("work".to_owned(), Some("alice".to_owned()))
        ]
    );
    sqlx::raw_sql(
        "SET rivet.defer_embed_enqueue = 'on';
         INSERT INTO ros_messages (conversation_id, agent, channel, role, content)
         VALUES ('00000000-0000-4000-8000-0000000000a2', 'kit', 'cli', 'user', 'a direct kit row')",
    )
    .execute(&pool)
    .await?;
    assert_eq!(owners(&pool, "rh_messages").await?, alice);
    assert!(
        sqlx::raw_sql(
            "SET rivet.defer_embed_enqueue = 'on';
             INSERT INTO rh_summaries (conversation_id, depth, content, kind, message_count)
             VALUES ('00000000-0000-4000-8000-0000000000a2', 0, 'unowned', 'leaf', 1)"
        )
        .execute(&pool)
        .await
        .is_err(),
        "a derived row without an owner is refused"
    );
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn a_store_without_a_single_owner_must_be_named(pool: PgPool) -> TestResult {
    before_owners(&pool).await?;
    sqlx::raw_sql(
        "INSERT INTO rh_conversations (session_key, agent, owner_user_id) VALUES
           ('cli:one', 'rivet', 'alice'), ('cli:two', 'rivet', 'bob'), ('cli:three', 'rivet', NULL)",
    )
    .execute(&pool)
    .await?;
    let Err(refused) = POSTGRES.run(&pool).await else {
        return Err("two owners and an unowned conversation must refuse".into());
    };
    assert!(refused.to_string().contains("rh.store_owner"), "{refused}");
    assert_eq!(
        owners(&pool, "rh_conversations").await?,
        vec![Some("alice".to_owned()), Some("bob".to_owned()), None]
    );

    owner::migrate_owned_by(&pool, "carol").await?;

    let owned: Vec<(String, String)> = sqlx::query_as(
        "SELECT session_key, owner_user_id FROM rh_conversations ORDER BY session_key",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        owned,
        vec![
            ("cli:one".to_owned(), "alice".to_owned()),
            ("cli:three".to_owned(), "carol".to_owned()),
            ("cli:two".to_owned(), "bob".to_owned()),
        ]
    );
    Ok(())
}

#[test]
fn every_row_of_a_file_takes_the_owner_of_its_store() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let conn = rusqlite::Connection::open(&path)?;
    conn.execute_batch(
        "INSERT INTO ros_conversations (id, session_key, agent, created_at, updated_at, owner_user_id)
           VALUES ('a1', 'cli:owned', 'rivet', '2026-10-30T00:00:00.000Z', '2026-10-30T00:00:00.000Z', 'alice'),
                  ('a2', 'cli:unowned', 'rivet', '2026-10-30T00:00:00.000Z', '2026-10-30T00:00:00.000Z', NULL);
         INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, created_at)
           VALUES ('m1', 'a2', 'rivet', 'cli', 'user', 'hello', '2026-10-30T00:00:00.000Z');
         INSERT INTO rh_topics (id, kind, slug, name, created_at, updated_at)
           VALUES ('t1', 'work', 'desk', 'Desk', '2026-10-30T00:00:00.000Z', '2026-10-30T00:00:00.000Z');
         INSERT INTO rh_session_placements (session_id, topic_id, placed_by, placed_at)
           VALUES ('cli:unowned', 't1', 'you', '2026-10-30T00:00:00.000Z');
         PRAGMA user_version = 27;",
    )?;
    drop(conn);

    migrate::sqlite(&path)?;

    let conn = rusqlite::Connection::open(&path)?;
    for (table, filter) in [
        ("ros_conversations", "1 = 1"),
        ("ros_messages", "1 = 1"),
        ("rh_topics", "kind = 'work'"),
        ("rh_session_placements", "1 = 1"),
    ] {
        let owners: Vec<Option<String>> = conn
            .prepare(&format!(
                "SELECT DISTINCT owner_user_id FROM {table} WHERE {filter}"
            ))?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        assert_eq!(owners, vec![Some("alice".to_owned())], "{table}");
    }
    let typed: i64 = conn.query_row(
        "SELECT count(*) FROM rh_topics WHERE kind = 'type' AND owner_user_id IS NOT NULL",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(typed, 0);
    Ok(())
}
