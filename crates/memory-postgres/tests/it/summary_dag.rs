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

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_archive_resolves_ranges_and_keeps_filtered_copy_closed(
    pool: PgPool,
) -> TestResult {
    use tokio_util::sync::CancellationToken;

    let fixture = std::fs::read_to_string(testkit::recording("summary-archive/top-down.sql"))?;
    let memory = PostgresMemory::new(pool.clone());
    let cancel = CancellationToken::new();
    memory.import_memory_dump(&fixture, false, &cancel).await?;
    let owner = SummaryOwner::Session("cli:top-down".to_owned());
    let section = assert_mixed_tree(&memory, &owner).await?;
    let deferred: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rh_summaries WHERE index_needs_repair")
            .fetch_one(&pool)
            .await?;
    assert_eq!(deferred, 2);

    let dump = export_fixture(&memory, &fixture, &cancel).await?;
    let target = crate::merge::source_database(&pool).await?;
    let imported = PostgresMemory::new(target.clone());
    let invalid = dump.replace(
        "00000000-0000-4000-8000-000000000103\t3",
        "00000000-0000-4000-8000-000000000103\t2",
    );
    assert!(matches!(
        imported.import_memory_dump(&invalid, true, &cancel).await,
        Err(memory_postgres::Error::DumpMessage(_))
    ));
    let report = imported.import_memory_dump(&dump, false, &cancel).await?;
    assert_eq!(report.rows.get("summaries").copied(), Some(4));
    assert_eq!(
        report.rows.get("messages").copied(),
        Some(4),
        "completed receipt adds its source"
    );
    assert_eq!(report.rows.get("summary_sources").copied(), Some(5));
    assert_eq!(report.rows.get("summary_passages").copied(), Some(1));
    assert_eq!(report.rows.get("summary_deliveries").copied(), Some(2));
    assert_eq!(
        imported.summary_node(&owner, &section.id).await?,
        Some(section)
    );

    let rejected = sqlx::query(
        "INSERT INTO rh_summaries (conversation_id, depth, content, kind, message_count, earliest_at,
             latest_at, pipeline_version, summary_ordinal, source_first, source_last, input_key, owner_user_id)
         SELECT conversation_id, depth, content, kind, message_count, earliest_at, latest_at,
             pipeline_version, summary_ordinal, source_first, source_last, input_key, owner_user_id
         FROM rh_summaries WHERE kind = 'session'",
    ).execute(&pool).await;
    assert!(
        matches!(rejected, Err(sqlx::Error::Database(error)) if error.constraint() == Some("rh_summary_one_session"))
    );
    Ok(())
}

async fn assert_mixed_tree(
    memory: &PostgresMemory,
    owner: &SummaryOwner,
) -> Result<kernel::memory::MemorySummaryNode, Box<dyn std::error::Error>> {
    let tree = memory
        .summary_tree(owner, None, TreeWindow::ALL)
        .await?
        .ok_or("expected tree")?;
    assert_eq!(
        (
            tree.counts.sessions,
            tree.counts.parts,
            tree.counts.sections
        ),
        (1, 1, 2)
    );
    assert_eq!(
        (tree.counts.leaves, tree.counts.branches, tree.counts.roots),
        (1, 1, 1)
    );
    let latest = memory.summary_latest(owner, 1).await?;
    let session = latest.summaries.first().ok_or("expected session")?;
    assert_eq!(session.kind, SummaryKind::Session);
    let children = memory
        .summary_tree(owner, Some(&session.id), TreeWindow::ALL)
        .await?
        .ok_or("expected children")?;
    let [section, part] = children.summaries.as_slice() else {
        return Err("expected a section and a part".into());
    };
    assert_eq!(
        (section.kind, part.kind),
        (SummaryKind::Section, SummaryKind::Part)
    );
    let section = memory
        .summary_node(owner, &section.id)
        .await?
        .ok_or("expected section")?;
    assert_eq!(
        section.source_message_ids,
        ["00000000-0000-4000-8000-000000000103"]
    );
    assert_eq!(
        (
            section.summary_ordinal,
            section.source_first,
            section.range_verified,
            section.quality_flags
        ),
        (Some(0), Some(3), Some(true), 1)
    );
    Ok(section)
}

async fn export_fixture(
    memory: &PostgresMemory,
    fixture: &str,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<String, Box<dyn std::error::Error>> {
    use kernel::memory::{DumpOrigin, DumpSource};
    use memory_postgres::dump::CopySource;
    let headers = fixture
        .lines()
        .filter_map(|line| line.strip_prefix("COPY public.ros_"))
        .filter_map(|line| line.strip_suffix(") FROM stdin;"))
        .filter_map(|line| line.split_once(" ("))
        .map(|(name, columns)| {
            (
                format!("rh_{name}"),
                format!("ros_{name}"),
                columns.split(", ").collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    let tables = headers
        .iter()
        .map(|(relation, compatibility, columns)| CopySource {
            relation,
            compatibility,
            columns,
        })
        .collect::<Vec<_>>();
    Ok(String::from_utf8(
        memory
            .export_memory_dump(
                &tables,
                Some(at("2026-10-11T00:00:00Z")),
                &DumpOrigin {
                    source: DumpSource::Datahub,
                    host: "fixture".to_owned(),
                },
                cancel,
            )
            .await?,
    )?)
}
