#![cfg(test)]

use chrono::DateTime;
use chrono::Utc;
use kernel::memory::MemoryMessageTally;
use kernel::memory::MemoryStore as _;
use kernel::memory::SummaryDag;
use kernel::memory::SummaryKind;
use kernel::memory::SummaryOwner;
use kernel::memory::TreeWindow;
use kernel::memory::VectorState;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SEED: &[&str] = &[
    "insert into rh_conversations (id, session_key, agent, channel, active, created_at, updated_at, owner_user_id) values
      ('00000000-0000-4000-8000-000000000001', 'cli:dag', 'rivet', 'cli', false, '2026-10-10T12:00:00Z', '2026-10-10T13:00:00Z', 'owner'),
      ('00000000-0000-4000-8000-000000000002', 'cli:dag', 'helper', 'cli', false, '2026-10-10T11:00:00Z', '2026-10-10T11:30:00Z', 'owner')",
    "insert into rh_messages (id, conversation_id, agent, channel, role, content, tool_name, embed_status, created_at, owner_user_id) values
      ('00000000-0000-4000-8000-000000000101', '00000000-0000-4000-8000-000000000001', 'rivet', 'cli', 'user', 'one', null, null, '2026-10-10T12:00:00Z', 'owner'),
      ('00000000-0000-4000-8000-000000000102', '00000000-0000-4000-8000-000000000001', 'rivet', 'cli', 'assistant', 'two', null, 'failed', '2026-10-10T12:01:00Z', 'owner'),
      ('00000000-0000-4000-8000-000000000103', '00000000-0000-4000-8000-000000000001', 'rivet', 'cli', 'assistant', 'three', 'Bash', 'unembeddable', '2026-10-10T12:02:00Z', 'owner'),
      ('00000000-0000-4000-8000-000000000104', '00000000-0000-4000-8000-000000000001', 'rivet', 'cli', 'tool', 'four', null, null, '2026-10-10T12:03:00Z', 'owner')",
    "insert into rh_summaries (id, conversation_id, depth, content, kind, message_count, earliest_at, latest_at, created_at, embed_status, owner_user_id) values
      ('00000000-0000-4000-8000-000000000014', '00000000-0000-4000-8000-000000000001', 1, 'branch', 'branch', 2, '2026-10-10T12:00:00Z', '2026-10-10T12:01:00Z', '2026-10-10T12:30:00Z', null, 'owner'),
      ('00000000-0000-4000-8000-000000000011', '00000000-0000-4000-8000-000000000001', 0, 'first', 'leaf', 1, '2026-10-10T12:00:00Z', '2026-10-10T12:00:00Z', '2026-10-10T12:30:00Z', 'failed', 'owner'),
      ('00000000-0000-4000-8000-000000000012', '00000000-0000-4000-8000-000000000001', 0, 'second', 'leaf', 1, '2026-10-10T12:01:00Z', '2026-10-10T12:01:00Z', '2026-10-10T12:30:00Z', 'unembeddable', 'owner'),
      ('00000000-0000-4000-8000-000000000015', '00000000-0000-4000-8000-000000000002', 0, 'stale', 'leaf', 0, '2026-10-10T11:00:00Z', '2026-10-10T11:00:00Z', '2026-10-10T11:30:00Z', null, 'owner')",
    "update rh_messages set embedding = array_fill(0.5::real, array[1024])::halfvec
      where id = '00000000-0000-4000-8000-000000000101'",
    "update rh_summaries set embedding = array_fill(0.5::real, array[1024])::halfvec
      where id = '00000000-0000-4000-8000-000000000014'",
    "update rh_summaries set parent_id = '00000000-0000-4000-8000-000000000014'
      where id in ('00000000-0000-4000-8000-000000000011', '00000000-0000-4000-8000-000000000012')",
    "insert into rh_summary_sources (summary_id, message_id, ordinal) values
      ('00000000-0000-4000-8000-000000000011', '00000000-0000-4000-8000-000000000101', 0),
      ('00000000-0000-4000-8000-000000000012', '00000000-0000-4000-8000-000000000102', 0)",
];

async fn seed(pool: &PgPool) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("select set_config('rivet.defer_embed_enqueue', 'on', true)")
        .execute(&mut *tx)
        .await?;
    for statement in SEED.iter().copied() {
        sqlx::query(statement).execute(&mut *tx).await?;
    }
    tx.commit().await
}

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .expect("timestamp")
        .with_timezone(&Utc)
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_ended_session_walks_its_summary_dag_and_tallies_its_messages(
    pool: PgPool,
) -> TestResult {
    seed(&pool).await?;
    let memory = PostgresMemory::new(pool);
    let dag = memory
        .summary_dag(&SummaryOwner::Session("cli:dag".to_owned()))
        .await?;
    let shape: Vec<_> = dag
        .summaries
        .iter()
        .map(|node| {
            (
                node.id.as_str(),
                node.parent_id.as_deref(),
                node.level,
                node.kind,
                node.vector,
            )
        })
        .collect();
    let branch = "00000000-0000-4000-8000-000000000014";
    assert_eq!(
        shape,
        vec![
            (branch, None, 1, SummaryKind::Branch, VectorState::Embedded),
            (
                "00000000-0000-4000-8000-000000000011",
                Some(branch),
                0,
                SummaryKind::Leaf,
                VectorState::Failed
            ),
            (
                "00000000-0000-4000-8000-000000000012",
                Some(branch),
                0,
                SummaryKind::Leaf,
                VectorState::Skipped
            ),
        ]
    );
    let first = dag.summaries.get(1).expect("first leaf");
    assert_eq!(
        first.source_message_ids,
        vec!["00000000-0000-4000-8000-000000000101"]
    );
    assert_eq!(first.message_count, 1);
    assert_eq!(first.content, "first");
    assert_eq!(first.earliest_at, Some(at("2026-10-10T12:00:00Z")));
    assert_eq!(first.created_at, at("2026-10-10T12:30:00Z"));
    assert_eq!(
        dag.summaries
            .first()
            .expect("branch")
            .source_message_ids
            .len(),
        0
    );
    assert_eq!(
        dag.messages,
        MemoryMessageTally {
            total: 4,
            user: 1,
            assistant: 2,
            system: 0,
            tool: 2,
            embedded: 1,
            pending: 1,
            failed: 1,
            skipped: 1,
        }
    );
    assert_eq!(
        memory
            .summary_dag(&SummaryOwner::Session("cli:missing".to_owned()))
            .await?,
        SummaryDag::default()
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_active_conversation_wins_over_a_newer_ended_one(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    sqlx::query(
        "update rh_conversations set active = true where id = '00000000-0000-4000-8000-000000000002'",
    )
    .execute(&pool)
    .await?;
    let dag = PostgresMemory::new(pool)
        .summary_dag(&SummaryOwner::Session("cli:dag".to_owned()))
        .await?;
    let ids: Vec<_> = dag.summaries.iter().map(|node| node.id.as_str()).collect();
    assert_eq!(ids, vec!["00000000-0000-4000-8000-000000000015"]);
    assert_eq!(dag.messages, MemoryMessageTally::default());
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_conversation_reads_its_own_summary_dag_by_id(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let memory = PostgresMemory::new(pool);
    let helper = uuid::uuid!("00000000-0000-4000-8000-000000000002");
    let dag = memory
        .summary_dag(&SummaryOwner::Conversation(helper))
        .await?;
    assert_eq!(dag.session_key.as_deref(), Some("cli:dag"));
    let ids: Vec<_> = dag.summaries.iter().map(|node| node.id.as_str()).collect();
    assert_eq!(ids, vec!["00000000-0000-4000-8000-000000000015"]);
    assert_eq!(
        memory
            .summary_dag(&SummaryOwner::Conversation(uuid::Uuid::nil()))
            .await?,
        SummaryDag::default()
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_outline_previews_each_summary_and_one_summary_reads_whole(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let long = format!("{}abcdé{}", "word ".repeat(63), " tail".repeat(40));
    sqlx::query(
        "update rh_summaries set content = $1 where id = '00000000-0000-4000-8000-000000000011'",
    )
    .bind(&long)
    .execute(&pool)
    .await?;
    let memory = PostgresMemory::new(pool);
    let owner = SummaryOwner::Session("cli:dag".to_owned());
    let full = memory.summary_dag(&owner).await?;
    let outline = memory.summary_outline(&owner).await?;
    assert_eq!(outline, full.clone().outline());
    let first = outline.summaries.get(1).expect("first leaf");
    assert!(
        first.truncated && long.starts_with(&first.content) && first.content.len() < long.len()
    );
    assert_eq!(first.source_message_ids, Vec::<String>::new());
    let whole = memory
        .summary_node(&owner, "00000000-0000-4000-8000-000000000011")
        .await?
        .expect("first leaf");
    assert_eq!(Some(&whole), full.summaries.get(1));
    assert_eq!(
        memory
            .summary_node(&owner, "00000000-0000-4000-8000-000000000015")
            .await?,
        None
    );
    let latest = memory.summary_latest(&owner, 1).await?;
    let [newest] = latest.summaries.as_slice() else {
        return Err("one summary".into());
    };
    assert_eq!(newest.id, "00000000-0000-4000-8000-000000000012");
    assert_eq!(newest.content, "second");
    assert_eq!(
        full.latest(1)
            .summaries
            .first()
            .map(|node| node.id.as_str()),
        Some(newest.id.as_str())
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_tree_reads_one_level_at_a_time_as_the_outline_would(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let memory = PostgresMemory::new(pool);
    let owner = SummaryOwner::Session("cli:dag".to_owned());
    let outline = memory.summary_outline(&owner).await?;
    let window = TreeWindow {
        offset: 0,
        limit: 1,
    };
    let top = memory.summary_tree(&owner, None, window).await?;
    assert_eq!(top, outline.clone().tree(None, window));
    let top = top.expect("the top");
    assert_eq!(
        (
            top.total,
            top.counts.top,
            top.counts.leaves,
            top.counts.embedded
        ),
        (1, 1, 2, 1)
    );
    let branch = "00000000-0000-4000-8000-000000000014";
    assert_eq!(
        memory
            .summary_tree(&owner, Some(branch), TreeWindow::ALL)
            .await?,
        outline.tree(Some(branch), TreeWindow::ALL)
    );
    assert_eq!(
        memory
            .summary_tree(
                &owner,
                Some("00000000-0000-4000-8000-000000000099"),
                TreeWindow::ALL
            )
            .await?,
        None
    );
    Ok(())
}
