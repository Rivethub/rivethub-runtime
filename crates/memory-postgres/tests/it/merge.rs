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

pub(super) async fn source_database(family: &PgPool) -> Result<PgPool, Box<dyn std::error::Error>> {
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

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_archive_merge_preserves_receipts_passages_and_the_winning_tree(
    pool: PgPool,
) -> TestResult {
    use kernel::memory::{MemoryStore as _, SummaryKind, SummaryOwner};

    let source_pool = source_database(&pool).await?;
    let family = PostgresMemory::new(pool.clone()).with_summary_index_key([0xab; 32]);
    let source = PostgresMemory::new(source_pool.clone()).with_summary_index_key([0xab; 32]);
    let fixture = std::fs::read_to_string(testkit::recording("summary-archive/top-down.txt"))?;
    let cancel = CancellationToken::new();
    source.import_memory_dump(&fixture, false, &cancel).await?;
    capture(&family, "alice", &[("cli:top-down", 1)]).await?;
    sqlx::query(
        "INSERT INTO rh_summaries (conversation_id, content, owner_user_id)
         SELECT id, 'Destination notes', owner_user_id FROM rh_conversations WHERE session_key = 'cli:top-down'",
    ).execute(&pool).await?;
    let request = MergeRequest {
        owner: Owner::new("alice".to_owned())?,
        moves: BTreeMap::new(),
        dry_run: false,
    };
    let deferred = family.merge_store(&source, &request, &cancel).await?;
    assert_eq!(deferred.kept_family, ["cli:top-down"]);
    assert_eq!(deferred.inserted.get("rh_summary_passages"), Some(&0));

    sqlx::raw_sql(
        "INSERT INTO rh_summary_passages (id, record_id, ordinal, char_start, char_end, content,
             earliest_at, latest_at, index_key)
         SELECT gen_random_uuid(), id, 0, 0, length(content), content, earliest_at, latest_at, input_key
         FROM rh_summaries WHERE kind = 'session';
         UPDATE rh_summary_passages SET embedding = array_fill(0.5::real, array[1024])::halfvec;
         UPDATE rh_summaries SET index_needs_repair = false;",
    ).execute(&source_pool).await?;
    let ready = family.merge_store(&source, &request, &cancel).await?;
    assert_eq!(ready.replaced, ["cli:top-down"]);
    assert_eq!(ready.dropped_legacy_summaries, 3);
    assert_eq!(ready.inserted.get("rh_summary_passages"), Some(&2));
    assert_eq!(ready.inserted.get("rh_summary_deliveries"), Some(&2));
    let dag = family
        .summary_dag(&SummaryOwner::Session("cli:top-down".to_owned()))
        .await?;
    assert_eq!(dag.summaries.len(), 4);
    assert!(
        dag.summaries
            .iter()
            .any(|node| node.kind == SummaryKind::Section)
    );
    let repeated = family.merge_store(&source, &request, &cancel).await?;
    assert_eq!(repeated.inserted.get("rh_summaries"), Some(&0));
    Ok(())
}

fn one_indexed_summary(node: Uuid, text: &str, specification: u8) -> String {
    let vector = format!("[{}0]", "0,".repeat(1023));
    let fingerprint = format!("{specification:02x}").repeat(32);
    let chars = text.chars().count();
    let conversation = Uuid::from_u128(0xc1);
    let message = Uuid::from_u128(0xc2);
    let passage = Uuid::from_u128(node.as_u128() + 100);
    format!(
        "COPY public.ros_conversations (id, session_key, agent, channel, owner_user_id) FROM stdin;
{conversation}\tcli:keep\tfixture\tcli\talice
\\.
COPY public.ros_messages (id, conversation_id, agent, channel, role, content, created_at, owner_user_id) FROM stdin;
{message}\t{conversation}\tfixture\tcli\tuser\tMeasure the spring.\t2024-01-01\talice
\\.
COPY public.ros_summaries (id, conversation_id, kind, depth, content, owner_user_id, message_count, earliest_at, latest_at, pipeline_version, summary_ordinal, source_first, source_last, input_key) FROM stdin;
{node}\t{conversation}\tsession\t2\t{text}\talice\t1\t2024-01-01\t2024-01-01\t7\t0\t1\t1\t\\\\x{}
\\.
COPY public.ros_summary_sources (summary_id, message_id, ordinal) FROM stdin;
{node}\t{message}\t1
\\.
COPY public.ros_summary_passages (id, record_id, ordinal, char_start, char_end, content, earliest_at, latest_at, index_key, embedding) FROM stdin;
{passage}\t{node}\t0\t0\t{chars}\t{text}\t2024-01-01\t2024-01-01\t\\\\x{fingerprint}\t{vector}
\\.
-- PostgreSQL database dump complete\n",
        "ab".repeat(32)
    )
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_foreign_model_passage_is_cleared_during_import(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(pool.clone()).with_summary_index_key([0xab; 32]);
    let root = Uuid::from_u128(0xc5);
    let dump = one_indexed_summary(root, "Alpha beta gamma.", 0xcd);
    let report = store
        .import_memory_dump(&dump, false, &CancellationToken::new())
        .await?;
    assert_eq!(report.summary_records_needing_reembedding, 1);
    let result: (bool, bool, bool) = sqlx::query_as(
        "SELECT rh_summary_record_indexed(s.id), s.index_needs_repair, p.embedding IS NULL
         FROM rh_summaries s JOIN rh_summary_passages p ON p.record_id = s.id WHERE s.id = $1",
    )
    .bind(root)
    .fetch_one(&pool)
    .await?;
    assert_eq!(result, (false, true, true));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_reports_a_complete_incoming_tree_with_a_different_id(pool: PgPool) -> TestResult {
    assert_keeps_destination_tree(pool, false).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_reports_an_incoming_tree_over_a_legacy_destination(pool: PgPool) -> TestResult {
    assert_keeps_destination_tree(pool, true).await
}

async fn assert_keeps_destination_tree(pool: PgPool, legacy: bool) -> TestResult {
    let source_pool = source_database(&pool).await?;
    let family = PostgresMemory::new(pool.clone()).with_summary_index_key([0xab; 32]);
    let source = PostgresMemory::new(source_pool.clone()).with_summary_index_key([0xab; 32]);
    let existing = Uuid::from_u128(0xa1);
    let incoming = Uuid::from_u128(0xa2);
    let cancel = CancellationToken::new();
    family
        .import_memory_dump(
            &one_indexed_summary(existing, "Destination session", 0xab),
            false,
            &cancel,
        )
        .await?;
    if legacy {
        sqlx::raw_sql("DELETE FROM rh_summary_passages; UPDATE rh_summaries SET kind = 'leaf', pipeline_version = 6")
            .execute(&pool)
            .await?;
    }
    source
        .import_memory_dump(
            &one_indexed_summary(incoming, "Incoming finished session", 0xab),
            false,
            &cancel,
        )
        .await?;
    if legacy {
        sqlx::raw_sql(
            "SET LOCAL rivet.defer_embed_enqueue = 'on';
             INSERT INTO rh_summaries (conversation_id, content, owner_user_id)
             SELECT id, 'Invented stale source leaf', owner_user_id FROM rh_conversations",
        )
        .execute(&source_pool)
        .await?;
    }
    let ready: bool = sqlx::query_scalar("SELECT rh_summary_record_indexed($1)")
        .bind(incoming)
        .fetch_one(&source_pool)
        .await?;
    assert!(ready);
    let report = family
        .merge_store(
            &source,
            &MergeRequest {
                owner: Owner::new("alice".to_owned())?,
                moves: BTreeMap::new(),
                dry_run: false,
            },
            &cancel,
        )
        .await?;
    assert_eq!(
        report.dropped_summary_trees,
        [memory_postgres::merge::DroppedSummaryTree {
            conversation: Uuid::from_u128(0xc1),
            session_key: "cli:keep".to_owned(),
            incoming_session: incoming,
            destination_session: (!legacy).then_some(existing),
        }]
    );
    assert_eq!(report.dropped_legacy_summaries, u64::from(legacy));
    let summary: (Uuid, String) = sqlx::query_as("SELECT id, content FROM rh_summaries")
        .fetch_one(&pool)
        .await?;
    assert_eq!(summary, (existing, "Destination session".to_owned()));
    assert_eq!(report.inserted.get("rh_summary_passages"), Some(&0));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn merge_accepts_a_store_with_eight_thousand_conversations(pool: PgPool) -> TestResult {
    let source = PostgresMemory::new(source_database(&pool).await?);
    let family = PostgresMemory::new(pool.clone());
    sqlx::query(
        "INSERT INTO rh_conversations (session_key, agent, owner_user_id)
         SELECT 'cli:bulk-' || n, 'fixture', 'alice' FROM generate_series(1, 8000) n",
    )
    .execute(&pool)
    .await?;
    let report = family
        .merge_store(
            &source,
            &MergeRequest {
                owner: Owner::new("alice".to_owned())?,
                moves: BTreeMap::new(),
                dry_run: true,
            },
            &CancellationToken::new(),
        )
        .await?;
    assert!(!report.committed);
    assert_eq!(report.runs, 1);
    Ok(())
}
