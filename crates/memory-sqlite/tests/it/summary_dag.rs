use crate::common;

use common::TestResult;
use common::execute;
use common::fresh_store;
use common::instant;
use kernel::memory::MemoryMessageTally;
use kernel::memory::MemoryStore;
use kernel::memory::MemorySummaryNode;
use kernel::memory::NewSummary;
use kernel::memory::SummaryDag;
use kernel::memory::SummaryKind;
use kernel::memory::SummaryOwner;
use kernel::memory::SummarySource;
use kernel::memory::SummaryStore;
use kernel::memory::VectorState;
use memory_sqlite::SqliteStore;
use uuid::Uuid;

const CURRENT: &str = "00000000-0000-4000-8000-000000000001";
const STALE: &str = "00000000-0000-4000-8000-000000000002";
const OTHER: &str = "00000000-0000-4000-8000-000000000003";
const LEAF_ONE: &str = "00000000-0000-4000-8000-000000000011";
const LEAF_TWO: &str = "00000000-0000-4000-8000-000000000012";
const BRANCH: &str = "00000000-0000-4000-8000-000000000013";
const ROOT: &str = "00000000-0000-4000-8000-000000000014";
const STALE_LEAF: &str = "00000000-0000-4000-8000-000000000015";
const OTHER_LEAF: &str = "00000000-0000-4000-8000-000000000016";

const SEED: &str = "
INSERT INTO ros_conversations (id, session_key, agent, channel, settings, active, created_at, updated_at) VALUES
  ('00000000-0000-4000-8000-000000000001', 'cli:dag', 'rivet', 'cli', '{}', 0, '2026-10-10T12:00:00.000Z', '2026-10-10T13:00:00.000Z'),
  ('00000000-0000-4000-8000-000000000002', 'cli:dag', 'helper', 'cli', '{}', 0, '2026-10-10T11:00:00.000Z', '2026-10-10T11:30:00.000Z'),
  ('00000000-0000-4000-8000-000000000003', 'cli:other', 'rivet', 'cli', '{}', 1, '2026-10-10T12:00:00.000Z', '2026-10-10T14:00:00.000Z');
INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, tool_name, metadata, created_at, embed_status, embedding) VALUES
  ('00000000-0000-4000-8000-000000000101', '00000000-0000-4000-8000-000000000001', 'rivet', 'cli', 'user', 'one', NULL, '{}', '2026-10-10T12:00:00.000Z', 'done', X'0000803F'),
  ('00000000-0000-4000-8000-000000000102', '00000000-0000-4000-8000-000000000001', 'rivet', 'cli', 'assistant', 'two', 'Bash', '{}', '2026-10-10T12:01:00.000Z', 'unembeddable', NULL),
  ('00000000-0000-4000-8000-000000000103', '00000000-0000-4000-8000-000000000001', 'rivet', 'cli', 'user', 'three', NULL, '{}', '2026-10-10T12:02:00.000Z', 'failed', NULL),
  ('00000000-0000-4000-8000-000000000104', '00000000-0000-4000-8000-000000000001', 'rivet', 'cli', 'tool', 'four', NULL, '{}', '2026-10-10T12:03:00.000Z', NULL, NULL);
";

const VECTORS: &str = "
UPDATE ros_summaries SET embedding = X'0000803F' WHERE id = '00000000-0000-4000-8000-000000000014';
UPDATE ros_summaries SET embed_status = 'failed' WHERE id = '00000000-0000-4000-8000-000000000013';
UPDATE ros_summaries SET embed_status = 'unembeddable' WHERE id = '00000000-0000-4000-8000-000000000011';
";

struct Planned<'a> {
    id: &'a str,
    conversation: &'a str,
    kind: SummaryKind,
    window: (&'a str, &'a str),
    sources: &'a [&'a str],
    children: &'a [&'a str],
}

async fn write(store: &SqliteStore, planned: Planned<'_>) -> TestResult {
    let mut sources = Vec::new();
    for (ordinal, id) in planned.sources.iter().enumerate() {
        sources.push(SummarySource {
            message_id: Uuid::parse_str(id)?,
            ordinal: i32::try_from(ordinal)?,
        });
    }
    let child_ids = planned
        .children
        .iter()
        .map(|id| Uuid::parse_str(id))
        .collect::<Result<Vec<_>, _>>()?;
    store
        .write_summary(NewSummary {
            id: Uuid::parse_str(planned.id)?,
            conversation_id: Uuid::parse_str(planned.conversation)?,
            kind: planned.kind,
            depth: planned.kind.depth(),
            content: format!("{} {}", planned.kind, planned.id),
            message_count: u32::try_from(planned.sources.len())?,
            earliest_at: instant(planned.window.0)?,
            latest_at: instant(planned.window.1)?,
            model: "fake-chat".to_owned(),
            pipeline_version: 5,
            created_at: instant("2026-10-10T12:30:00.000Z")?,
            sources,
            child_ids,
        })
        .await?;
    Ok(())
}

fn leaf<'a>(
    id: &'a str,
    conversation: &'a str,
    window: (&'a str, &'a str),
    sources: &'a [&'a str],
) -> Planned<'a> {
    Planned {
        id,
        conversation,
        kind: SummaryKind::Leaf,
        window,
        sources,
        children: &[],
    }
}

async fn seeded() -> TestResult<common::Fresh> {
    let fresh = fresh_store()?;
    execute(&fresh.store, SEED).await?;
    let first = ("2026-10-10T12:00:00.000Z", "2026-10-10T12:01:00.000Z");
    let second = ("2026-10-10T12:02:00.000Z", "2026-10-10T12:02:00.000Z");
    let ones = [
        "00000000-0000-4000-8000-000000000101",
        "00000000-0000-4000-8000-000000000102",
    ];
    let twos = ["00000000-0000-4000-8000-000000000103"];
    write(&fresh.store, leaf(LEAF_TWO, CURRENT, second, &twos)).await?;
    write(&fresh.store, leaf(LEAF_ONE, CURRENT, first, &ones)).await?;
    write(&fresh.store, leaf(STALE_LEAF, STALE, first, &[])).await?;
    write(&fresh.store, leaf(OTHER_LEAF, OTHER, first, &[])).await?;
    let span = ("2026-10-10T12:00:00.000Z", "2026-10-10T12:02:00.000Z");
    let branch = Planned {
        id: BRANCH,
        conversation: CURRENT,
        kind: SummaryKind::Branch,
        window: span,
        sources: &[],
        children: &[LEAF_ONE, LEAF_TWO],
    };
    write(&fresh.store, branch).await?;
    let root = Planned {
        id: ROOT,
        conversation: CURRENT,
        kind: SummaryKind::Root,
        window: span,
        sources: &[],
        children: &[BRANCH],
    };
    write(&fresh.store, root).await?;
    execute(&fresh.store, VECTORS).await?;
    Ok(fresh)
}

type Shape<'a> = (&'a str, Option<&'a str>, u32, SummaryKind, VectorState);

fn shape(nodes: &[MemorySummaryNode]) -> Vec<Shape<'_>> {
    nodes
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
        .collect()
}

#[tokio::test]
async fn an_ended_session_walks_its_summary_dag_and_tallies_its_messages() -> TestResult {
    let fresh = seeded().await?;
    let dag = fresh
        .store
        .summary_dag(&SummaryOwner::Session("cli:dag".to_owned()))
        .await?;
    assert_eq!(
        shape(&dag.summaries),
        vec![
            (ROOT, None, 2, SummaryKind::Root, VectorState::Embedded),
            (
                BRANCH,
                Some(ROOT),
                1,
                SummaryKind::Branch,
                VectorState::Failed
            ),
            (
                LEAF_ONE,
                Some(BRANCH),
                0,
                SummaryKind::Leaf,
                VectorState::Skipped
            ),
            (
                LEAF_TWO,
                Some(BRANCH),
                0,
                SummaryKind::Leaf,
                VectorState::Pending
            ),
        ]
    );
    assert_eq!(
        dag.messages,
        MemoryMessageTally {
            total: 4,
            user: 2,
            assistant: 1,
            system: 0,
            tool: 2,
            embedded: 1,
            pending: 1,
            failed: 1,
            skipped: 1,
        }
    );
    let first = dag.summaries.get(2).ok_or("first leaf")?;
    assert_eq!(
        first.source_message_ids,
        vec![
            "00000000-0000-4000-8000-000000000101",
            "00000000-0000-4000-8000-000000000102",
        ]
    );
    assert_eq!(first.message_count, 2);
    assert_eq!(first.content, format!("leaf {LEAF_ONE}"));
    assert_eq!(
        first.earliest_at,
        Some(instant("2026-10-10T12:00:00.000Z")?)
    );
    assert_eq!(first.latest_at, Some(instant("2026-10-10T12:01:00.000Z")?));
    assert_eq!(first.created_at, instant("2026-10-10T12:30:00.000Z")?);
    let root = dag.summaries.first().ok_or("root")?;
    assert_eq!(root.source_message_ids.len(), 0);
    Ok(())
}

#[tokio::test]
async fn the_summary_dag_is_empty_for_a_session_without_a_conversation() -> TestResult {
    let fresh = seeded().await?;
    assert_eq!(
        fresh
            .store
            .summary_dag(&SummaryOwner::Session("cli:missing".to_owned()))
            .await?,
        SummaryDag::default()
    );
    let other = fresh
        .store
        .summary_dag(&SummaryOwner::Session("cli:other".to_owned()))
        .await?;
    assert_eq!(
        shape(&other.summaries),
        vec![(OTHER_LEAF, None, 0, SummaryKind::Leaf, VectorState::Pending)]
    );
    Ok(())
}

#[tokio::test]
async fn an_active_conversation_wins_over_a_newer_ended_one() -> TestResult {
    let fresh = seeded().await?;
    execute(
        &fresh.store,
        "UPDATE ros_conversations SET active = 1 WHERE id = '00000000-0000-4000-8000-000000000002'",
    )
    .await?;
    let dag = fresh
        .store
        .summary_dag(&SummaryOwner::Session("cli:dag".to_owned()))
        .await?;
    assert_eq!(
        shape(&dag.summaries),
        vec![(STALE_LEAF, None, 0, SummaryKind::Leaf, VectorState::Pending)]
    );
    assert_eq!(dag.messages, MemoryMessageTally::default());
    Ok(())
}

#[tokio::test]
async fn a_conversation_reads_its_own_summary_dag_by_id() -> TestResult {
    let fresh = seeded().await?;
    let stale = fresh
        .store
        .summary_dag(&SummaryOwner::Conversation(Uuid::parse_str(STALE)?))
        .await?;
    assert_eq!(stale.session_key.as_deref(), Some("cli:dag"));
    assert_eq!(
        shape(&stale.summaries),
        vec![(STALE_LEAF, None, 0, SummaryKind::Leaf, VectorState::Pending)]
    );
    assert_eq!(
        fresh
            .store
            .summary_dag(&SummaryOwner::Conversation(Uuid::nil()))
            .await?,
        SummaryDag::default()
    );
    Ok(())
}

#[tokio::test]
async fn top_down_archive_keeps_the_full_filtered_tree_without_postgres_passages() -> TestResult {
    use kernel::memory::{DumpOrigin, DumpSource};
    use memory_sqlite::import::ImportTable;
    use tokio_util::sync::CancellationToken;

    let source = fresh_store()?;
    let cancel = CancellationToken::new();
    let fixture = testkit::recording("summary-archive/top-down.txt");
    let imported = source.store.import_pg_dump(fixture, cancel.clone()).await?;
    assert_eq!(imported.rows.get(&ImportTable::Summaries).copied(), Some(7));
    let owner = SummaryOwner::Session("cli:top-down".to_owned());
    let section = assert_mixed_tree(&source.store, &owner).await?;
    let duplicate = execute(&source.store, "
        INSERT INTO ros_summaries (id, conversation_id, kind, created_at)
        SELECT 'duplicate', conversation_id, kind, created_at FROM ros_summaries WHERE kind = 'session'
    ").await;
    assert!(duplicate.is_err());

    let bytes = source
        .store
        .export_pg_dump(
            Some(instant("2026-10-11T00:00:00Z")?),
            DumpOrigin {
                source: DumpSource::Local,
                host: "fixture".to_owned(),
            },
            cancel.clone(),
        )
        .await?;
    let filtered = tempfile::NamedTempFile::new()?;
    std::fs::write(filtered.path(), &bytes)?;
    let target = fresh_store()?;
    let report = target
        .store
        .import_pg_dump(filtered.path().to_owned(), cancel.clone())
        .await?;
    assert_eq!(report.rows.get(&ImportTable::Summaries).copied(), Some(4));
    assert_eq!(
        report.rows.get(&ImportTable::Messages).copied(),
        Some(4),
        "completed receipt adds its source"
    );
    assert_eq!(
        report.rows.get(&ImportTable::SummaryDeliveries).copied(),
        Some(2)
    );
    assert_eq!(
        report.rows.get(&ImportTable::SummarySources).copied(),
        Some(5)
    );
    assert_eq!(
        target.store.summary_node(&owner, &section.id).await?,
        Some(section)
    );

    let valid = String::from_utf8(bytes)?;
    std::fs::write(
        filtered.path(),
        valid.replace(
            "00000000-0000-4000-8000-000000000103\t3",
            "00000000-0000-4000-8000-000000000103\t2",
        ),
    )?;
    let invalid = fresh_store()?;
    assert!(matches!(
        invalid
            .store
            .preview_pg_dump(filtered.path().to_owned(), cancel)
            .await,
        Err(memory_sqlite::import::ImportError::SummaryGraph)
    ));
    Ok(())
}

async fn assert_mixed_tree(
    store: &SqliteStore,
    owner: &SummaryOwner,
) -> TestResult<MemorySummaryNode> {
    use kernel::memory::TreeWindow;
    let tree = store
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
    let latest = store.summary_latest(owner, 1).await?;
    let session = latest.summaries.first().ok_or("expected session")?;
    assert_eq!(session.kind, SummaryKind::Session);
    let root = &session.id;
    let children = store
        .summary_tree(owner, Some(root), TreeWindow::ALL)
        .await?
        .ok_or("expected children")?;
    let [section, part] = children.summaries.as_slice() else {
        return Err("expected a section and a part".into());
    };
    assert_eq!(
        (section.kind, part.kind),
        (SummaryKind::Section, SummaryKind::Part)
    );
    let section = store
        .summary_node(owner, &section.id)
        .await?
        .ok_or("expected section")?;
    assert_eq!(
        section.source_message_ids,
        ["00000000-0000-4000-8000-000000000103"]
    );
    assert_eq!(
        (
            section.source_first,
            section.source_last,
            section.range_verified,
            section.quality_flags
        ),
        (Some(3), Some(3), Some(true), 1)
    );
    Ok(section)
}

#[tokio::test]
async fn top_down_foreign_model_pooled_vector_is_cleared_during_import() -> TestResult {
    let local = fresh_store()?;
    let store = local.store.with_summary_index_key([0xab; 32]);
    let dump = tempfile::NamedTempFile::new()?;
    let root = uuid::Uuid::from_u128(0xc5);
    let conversation = uuid::Uuid::from_u128(0xc1);
    let message = uuid::Uuid::from_u128(0xc2);
    let vector = format!("[{}1]", "0,".repeat(1023));
    std::fs::write(dump.path(), format!(
        "COPY public.ros_conversations (id, session_key, agent, channel, created_at, updated_at, owner_user_id) FROM stdin;
{conversation}\tcli:keep\tfixture\tcli\t2024-01-01T00:00:00Z\t2024-01-01T00:00:00Z\talice
\\.
COPY public.ros_messages (id, conversation_id, agent, channel, role, content, created_at) FROM stdin;
{message}\t{conversation}\tfixture\tcli\tuser\tMeasure the spring.\t2024-01-01T00:00:00Z
\\.
COPY public.ros_summaries (id, conversation_id, kind, depth, content, message_count, earliest_at, latest_at, created_at, pipeline_version, summary_ordinal, source_first, source_last, input_key, index_key, embedding) FROM stdin;
{root}\t{conversation}\tsession\t2\tAlpha beta gamma.\t1\t2024-01-01T00:00:00Z\t2024-01-01T00:00:00Z\t2024-01-01T00:00:00Z\t7\t0\t1\t1\t\\\\x{}\t\\\\x{}\t{vector}
\\.
COPY public.ros_summary_sources (summary_id, message_id, ordinal) FROM stdin;
{root}\t{message}\t1
\\.
-- PostgreSQL database dump complete\n",
        "ab".repeat(32), "cd".repeat(32),
    ))?;
    let report = store
        .import_pg_dump(
            dump.path().to_path_buf(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await?;
    assert_eq!(report.summary_records_needing_reembedding, 1);
    let result = store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT index_needs_repair, embedding IS NULL, index_key FROM ros_summaries",
                [],
                |row| {
                    Ok((
                        row.get::<_, bool>(0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )
            .map_err(|source| memory_sqlite::SqliteError::Query {
                operation: "test index specification",
                source,
            })
        })
        .await?;
    assert_eq!(result, (true, true, vec![0xab; 32]));
    Ok(())
}
