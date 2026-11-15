use std::collections::BTreeMap;

use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use kernel::owner::Owner;
use memory_postgres::CaptureOptions;
use memory_postgres::PostgresMemory;
use memory_postgres::merge::MergeRequest;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use wire::message::Role;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const FAMILY_ONLY: &str = "claude-code:00000000-0000-4000-8000-0000000000d1";
const BOTH_SMALLER: &str = "claude-code:00000000-0000-4000-8000-0000000000d2";
const BOTH_LARGER: &str = "claude-code:00000000-0000-4000-8000-0000000000d3";
const CLEO_ONLY: &str = "claude-code:00000000-0000-4000-8000-0000000000d4";
const DANAS_IN_CLEO: &str = "claude-code:00000000-0000-4000-8000-0000000000d5";

fn batch(session: &str, count: usize) -> CaptureBatchRequest {
    CaptureBatchRequest {
        session_key: session.to_owned(),
        agent: "rivet".to_owned(),
        channel: Some("cli".to_owned()),
        title: None,
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages: (0..count)
            .map(|n| CaptureMessage {
                event_id: format!("{session}-{n}"),
                role: Role::User,
                content: format!("message {n} of {session}"),
                tool_name: None,
                tool_args: None,
                tool_result: None,
                metadata: None,
                created_at: None,
            })
            .collect(),
    }
}

async fn capture(
    store: &PostgresMemory,
    owner: &str,
    sessions: &[(&str, usize)],
) -> Result<(), Box<dyn std::error::Error>> {
    let owner = Owner::new(owner.to_owned())?;
    for (session, count) in sessions {
        store
            .capture_batch(&batch(session, *count), &CaptureOptions::owned_by(&owner))
            .await?;
    }
    Ok(())
}

async fn source_database(family: &PgPool) -> Result<PgPool, Box<dyn std::error::Error>> {
    let source: String =
        sqlx::query_scalar("SELECT 'rh_merge_' || substr(md5(current_database()), 1, 16)")
            .fetch_one(family)
            .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS \"{source}\""
    )))
    .execute(family)
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE \"{source}\"")))
        .execute(family)
        .await?;
    let options = family.connect_options().as_ref().clone().database(&source);
    let pool = memory_postgres::scoped(PgPoolOptions::new().max_connections(2))
        .connect_with(options)
        .await?;
    migrate::POSTGRES.run(&pool).await?;
    Ok(pool)
}

async fn rows(pool: &PgPool, sql: &str) -> Result<Vec<(String, String)>, sqlx::Error> {
    sqlx::query_as(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_all(pool)
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_second_store_merges_in_private_under_its_owner(pool: PgPool) -> TestResult {
    let source_pool = source_database(&pool).await?;
    let family = PostgresMemory::new(pool.clone());
    let source = PostgresMemory::new(source_pool.clone());
    capture(
        &family,
        "dana",
        &[(FAMILY_ONLY, 1), (BOTH_SMALLER, 3), (BOTH_LARGER, 1)],
    )
    .await?;
    capture(
        &source,
        "cleo",
        &[
            (BOTH_SMALLER, 2),
            (BOTH_LARGER, 4),
            (CLEO_ONLY, 2),
            (DANAS_IN_CLEO, 1),
        ],
    )
    .await?;
    let moved: Uuid = sqlx::query_scalar("SELECT id FROM rh_conversations WHERE session_key = $1")
        .bind(DANAS_IN_CLEO)
        .fetch_one(&source_pool)
        .await?;
    sqlx::query(
        "INSERT INTO rh_wiki_topics (slug, title, current_state, article, search_text, created_at, updated_at, owner_user_id)
         VALUES ('sourdough', 'Sourdough', 'Fed twice a day.', '', 'sourdough', now(), now(), 'cleo')",
    )
    .execute(&source_pool)
    .await?;
    let request = MergeRequest {
        owner: Owner::new("cleo".to_owned())?,
        moves: BTreeMap::from([(moved, Owner::new("dana".to_owned())?)]),
        dry_run: false,
    };
    let cancel = CancellationToken::new();
    let report = family.merge_store(&source, &request, &cancel).await?;
    assert_eq!(report.kept_family, vec![BOTH_SMALLER.to_owned()]);
    assert_eq!(report.replaced, vec![BOTH_LARGER.to_owned()]);
    assert_eq!(report.moved_conversations, 1);
    let found = rows(
        &pool,
        "SELECT c.session_key || ' ' || c.owner_user_id || ' ' || c.visibility,
                (SELECT count(*) FROM rh_messages m WHERE m.conversation_id = c.id)::text
           FROM rh_conversations c ORDER BY c.session_key",
    )
    .await?;
    assert_eq!(
        found,
        vec![
            (format!("{FAMILY_ONLY} dana private"), "1".to_owned()),
            (format!("{BOTH_SMALLER} dana private"), "3".to_owned()),
            (format!("{BOTH_LARGER} dana private"), "4".to_owned()),
            (format!("{CLEO_ONLY} cleo private"), "2".to_owned()),
            (format!("{DANAS_IN_CLEO} dana private"), "1".to_owned()),
        ]
    );
    let owners = rows(
        &pool,
        "SELECT owner_user_id, count(*)::text FROM rh_messages GROUP BY 1 ORDER BY 1",
    )
    .await?;
    assert_eq!(
        owners,
        vec![
            ("cleo".to_owned(), "2".to_owned()),
            ("dana".to_owned(), "9".to_owned())
        ]
    );
    let types = rows(
        &pool,
        "SELECT kind, count(*)::text FROM rh_topics GROUP BY 1 ORDER BY 1",
    )
    .await?;
    let seeded = rows(
        &source_pool,
        "SELECT kind, count(*)::text FROM rh_topics GROUP BY 1 ORDER BY 1",
    )
    .await?;
    assert_eq!(
        types, seeded,
        "seeded type topics join the family's instead of doubling"
    );

    let pages = rows(
        &pool,
        "SELECT t.slug || ' ' || t.local_slug, s.directory || ' ' || t.owner_user_id
           FROM rh_wiki_topics t JOIN rh_wiki_scopes s ON s.scope = t.scope",
    )
    .await?;
    assert_eq!(
        pages,
        vec![(
            "cleo-sourdough sourdough".to_owned(),
            "users/cleo cleo".to_owned()
        )],
        "a merged page becomes its owner's page under its original file name"
    );

    let again = family.merge_store(&source, &request, &cancel).await?;
    assert_eq!(again.runs, 2);
    assert!(
        again.inserted.values().all(|added| *added == 0),
        "{:?}",
        again.inserted
    );
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&source_pool)
        .await?;
    source_pool.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE \"{name}\"")))
        .execute(&pool)
        .await?;
    Ok(())
}
