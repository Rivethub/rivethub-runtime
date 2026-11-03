#![cfg(test)]

use chrono::DateTime;
use chrono::Utc;
use kernel::memory::NewSummary;
use kernel::memory::SummaryKind;
use kernel::memory::SummarySource;
use kernel::memory::SummaryStore;
use kernel::memory::SummaryWatch as _;
use kernel::memory::SummaryWrite;
use kernel::memory::ToolUse;
use memory_postgres::PostgresMemory;
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, sqlx::FromRow)]
struct StoredSummary {
    parent_id: Option<Uuid>,
    kind: String,
    message_count: i32,
    model: Option<String>,
    pipeline_version: i32,
    embed_status: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct StoredSource {
    message_id: Uuid,
    ordinal: i32,
}

#[derive(Debug, sqlx::FromRow)]
struct EmbedJob {
    identifier: String,
    key: Option<String>,
    max_attempts: i64,
    payload: String,
}

#[derive(Debug, Deserialize)]
struct EmbedPayload {
    #[serde(rename = "targetTable")]
    target_table: String,
    #[serde(rename = "targetId")]
    target_id: String,
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn summary_writes_link_children_and_let_the_embed_trigger_fire(pool: PgPool) -> TestResult {
    if !testkit::queue_present(&pool).await? {
        return Ok(());
    }
    let memory = PostgresMemory::new(pool.clone());
    let conversation = seed_conversation(&pool, "cli:summary", "Session cli:summary").await?;
    let heartbeat = seed_conversation(&pool, "heartbeat:pulse", "heartbeat").await?;
    let first = seed_message(&pool, conversation, "one", at("2026-10-10T12:00:00Z")).await?;
    let second = seed_message(&pool, conversation, "two", at("2026-10-10T12:01:00Z")).await?;
    seed_message(&pool, heartbeat, "beat", at("2026-10-10T12:00:00Z")).await?;

    let pending = memory.pending().await?;
    assert_eq!(pending.len(), 1);
    let head = pending.first().expect("pending conversation");
    assert_eq!(head.id, conversation);
    assert_eq!(head.unsummarized, 2);
    assert_eq!(head.latest_message_at, Some(at("2026-10-10T12:01:00Z")));

    let leaf_id = Uuid::new_v4();
    let created = at("2026-10-10T12:12:00Z");
    memory
        .write_summary(NewSummary {
            id: leaf_id,
            conversation_id: conversation,
            kind: SummaryKind::Leaf,
            depth: SummaryKind::Leaf.depth(),
            content: "leaf text".to_owned(),
            message_count: 1,
            earliest_at: at("2026-10-10T12:00:00Z"),
            latest_at: at("2026-10-10T12:00:00Z"),
            model: "fake-chat".to_owned(),
            pipeline_version: 5,
            created_at: created,
            sources: vec![SummarySource {
                message_id: first,
                ordinal: 0,
            }],
            child_ids: Vec::new(),
        })
        .await?;

    let loaded = memory
        .conversation(conversation)
        .await?
        .ok_or("conversation")?;
    assert_eq!(loaded.messages.len(), 1);
    assert_eq!(loaded.messages.first().expect("message").id, second);
    assert_eq!(loaded.leaves.len(), 1);
    assert_eq!(loaded.leaves.first().expect("leaf").id, leaf_id);
    assert_eq!(loaded.branches.len(), 0);

    let branch_id = Uuid::new_v4();
    memory
        .write_summary(NewSummary {
            id: branch_id,
            conversation_id: conversation,
            kind: SummaryKind::Branch,
            depth: SummaryKind::Branch.depth(),
            content: "branch text".to_owned(),
            message_count: 1,
            earliest_at: at("2026-10-10T12:00:00Z"),
            latest_at: at("2026-10-10T12:00:00Z"),
            model: "fake-chat".to_owned(),
            pipeline_version: 5,
            created_at: created,
            sources: Vec::new(),
            child_ids: vec![leaf_id],
        })
        .await?;

    let loaded = memory
        .conversation(conversation)
        .await?
        .ok_or("conversation")?;
    assert_eq!(loaded.leaves.len(), 0);
    assert_eq!(loaded.branches.len(), 1);
    assert_eq!(loaded.branches.first().expect("branch").id, branch_id);
    let stored = sqlx::query_as::<_, StoredSummary>(
        "select parent_id, kind, message_count, model, pipeline_version, embed_status
           from rh_summaries where id = $1",
    )
    .bind(leaf_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(stored.parent_id, Some(branch_id));
    assert_eq!(stored.kind, "leaf");
    assert_eq!(stored.message_count, 1);
    assert_eq!(stored.model.as_deref(), Some("fake-chat"));
    assert_eq!(stored.pipeline_version, 5);
    assert_eq!(stored.embed_status, None);
    let sources = sqlx::query_as::<_, StoredSource>(
        "select message_id, ordinal from rh_summary_sources where summary_id = $1 order by ordinal",
    )
    .bind(leaf_id)
    .fetch_all(&pool)
    .await?;
    assert_eq!(sources.len(), 1);
    let source = sources.first().expect("source");
    assert_eq!(source.message_id, first);
    assert_eq!(source.ordinal, 0);

    let key = format!("embed-ros_summaries-{leaf_id}");
    let job = sqlx::query_as::<_, EmbedJob>(
        "select t.identifier, j.key, j.max_attempts::bigint as max_attempts, j.payload::text as payload
           from graphile_worker._private_jobs j
           join graphile_worker._private_tasks t on t.id = j.task_id
          where j.key = $1",
    )
    .bind(&key)
    .fetch_one(&pool)
    .await?;
    assert_eq!(job.identifier, "embed-target");
    assert_eq!(job.key.as_deref(), Some(key.as_str()));
    assert_eq!(job.max_attempts, 5);
    let payload: EmbedPayload = serde_json::from_str(&job.payload)?;
    assert_eq!(payload.target_table, "ros_summaries");
    assert_eq!(payload.target_id, leaf_id.to_string());
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn pending_lists_the_most_recent_activity_first(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    let mut seeded = Vec::new();
    for (session, latest) in [
        ("cli:old", "2026-10-11T09:00:00Z"),
        ("cli:recent", "2026-10-30T17:00:00Z"),
        ("cli:week", "2026-10-24T12:00:00Z"),
    ] {
        let id = seed_conversation(&pool, session, session).await?;
        seed_message(&pool, id, "earlier", at("2026-10-01T08:00:00Z")).await?;
        seed_message(&pool, id, "latest", at(latest)).await?;
        seeded.push((id, at(latest)));
    }
    seeded.sort_by_key(|(_, latest)| std::cmp::Reverse(*latest));
    let pending: Vec<_> = memory
        .pending()
        .await?
        .into_iter()
        .map(|conversation| {
            (
                conversation.id,
                conversation.latest_message_at,
                conversation.unsummarized,
            )
        })
        .collect();
    let expected: Vec<_> = seeded
        .into_iter()
        .map(|(id, latest)| (id, Some(latest), 2))
        .collect();
    assert_eq!(pending, expected);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_tool_row_shows_its_clipped_input_and_result(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    let conversation = seed_conversation(&pool, "cli:tools", "Session cli:tools").await?;
    seed_message(&pool, conversation, "run it", at("2026-10-10T12:00:00Z")).await?;
    let command = "\u{e9}".repeat(400);
    let output = "line\n".repeat(200);
    let mut tx = deferred(&pool).await?;
    sqlx::query(
        "insert into rh_messages (conversation_id, agent, channel, role, content, tool_name,
                                  tool_args, tool_result, created_at, owner_user_id)
         values ($1, 'rivet', 'cli', 'tool', '[tool call] Bash', 'Bash',
                 jsonb_build_object('command', $2::text), $3, $4, 'owner')",
    )
    .bind(conversation)
    .bind(&command)
    .bind(&output)
    .bind(at("2026-10-10T12:01:00Z"))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let window = memory
        .conversation(conversation)
        .await?
        .ok_or("conversation window")?;
    let tools: Vec<Option<ToolUse>> = window
        .messages
        .into_iter()
        .map(|message| message.tool)
        .collect();
    let [None, Some(tool)] = tools.as_slice() else {
        return Err("one tool row after the user message".into());
    };
    let args = format!("{{\"command\": \"{command}\"}}");
    let input = tool
        .input
        .strip_suffix(" \u{2026}")
        .ok_or("clipped input")?;
    assert!(input.len() <= ToolUse::INPUT_BYTES && args.starts_with(input));
    let shown = tool
        .result
        .strip_suffix(" \u{2026}")
        .ok_or("clipped result")?;
    assert!(shown.len() <= ToolUse::RESULT_BYTES && output.starts_with(shown));
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct StoredTitle {
    title: Option<String>,
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_written_summary_is_announced_to_watchers(pool: PgPool) -> TestResult {
    sqlx::query("alter table rh_summaries disable trigger user")
        .execute(&pool)
        .await?;
    let memory = PostgresMemory::new(pool.clone());
    let conversation = seed_conversation(&pool, "cli:announced", "Session cli:announced").await?;
    let listening = tokio_util::sync::CancellationToken::new();
    let watch = memory.spawn_summary_watch(listening.clone()).await?;
    let mut written = memory.written();
    let leaf = titled_leaf(conversation, "Announced to the plug-ins.");
    let leaf_id = leaf.id;
    memory.write_summary(leaf).await?;
    let heard = tokio::time::timeout(std::time::Duration::from_secs(10), written.recv()).await??;
    assert_eq!(
        (
            heard.id,
            heard.session_key.as_str(),
            heard.kind,
            heard.depth
        ),
        (leaf_id, "cli:announced", SummaryKind::Leaf, 0)
    );
    listening.cancel();
    watch.await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn the_first_leaf_titles_only_conversations_without_a_real_title(pool: PgPool) -> TestResult {
    if !testkit::queue_present(&pool).await? {
        return Ok(());
    }
    let memory = PostgresMemory::new(pool.clone());
    let untitled = seed_untitled(&pool, "cli:untitled").await?;
    let named = seed_conversation(&pool, "cli:named", "Named by the hook").await?;
    let placeholder = seed_conversation(&pool, "cli:placeholder", "dsh session").await?;
    let first = memory
        .write_summary(titled_leaf(
            untitled,
            "Moved the build to the new host. Then slept.",
        ))
        .await?;
    assert_eq!(first, SummaryWrite::FirstLeaf);
    let later = memory
        .write_summary(titled_leaf(untitled, "A later leaf about something else."))
        .await?;
    assert_eq!(later, SummaryWrite::Added);
    memory
        .write_summary(titled_leaf(named, "Moved the build to the new host."))
        .await?;
    assert_eq!(
        stored_title(&pool, untitled).await?.as_deref(),
        Some("Moved the build to the new host.")
    );
    assert_eq!(
        stored_title(&pool, named).await?.as_deref(),
        Some("Named by the hook")
    );
    memory
        .write_summary(titled_leaf(placeholder, "### Port the parser"))
        .await?;
    assert_eq!(
        stored_title(&pool, placeholder).await?.as_deref(),
        Some("Port the parser")
    );
    sqlx::query("update rh_conversations set title = null where id = $1")
        .bind(untitled)
        .execute(&pool)
        .await?;
    sqlx::query("update rh_conversations set title = 'dsh session' where id = $1")
        .bind(placeholder)
        .execute(&pool)
        .await?;
    assert_eq!(memory.backfill_titles(true).await?, 2);
    assert_eq!(stored_title(&pool, untitled).await?, None);
    assert_eq!(memory.backfill_titles(false).await?, 2);
    assert_eq!(
        stored_title(&pool, untitled).await?.as_deref(),
        Some("Moved the build to the new host.")
    );
    assert_eq!(
        stored_title(&pool, placeholder).await?.as_deref(),
        Some("Port the parser")
    );
    assert_eq!(memory.backfill_titles(false).await?, 0);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn model_titles_replace_only_weak_or_missing_titles(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    let weak = seed_conversation(&pool, "cli:weak", "Session cli:weak").await?;
    let named = seed_conversation(&pool, "cli:named", "Named by the hook").await?;
    let sandbox = seed_conversation(
        &pool,
        "grokbot-run-sand-subagent-00000000-0000-4000-8000-000000000001",
        "Session grokbot-run-sand-subagent-00000000-0000-4000-8000-000000000001",
    )
    .await?;
    let bare = seed_untitled(&pool, "cli:bare").await?;
    for (id, content) in [
        (weak, "**Kimi Code session start**\n- Began."),
        (weak, "A second leaf."),
        (named, "Moved the build."),
        (sandbox, "Ran a sandbox check."),
    ] {
        seed_leaf(&pool, id, content).await?;
    }
    let untitled = memory.untitled(1).await?;
    let listed: Vec<(Uuid, usize)> = untitled
        .iter()
        .map(|item| (item.head.id, item.leaves.len()))
        .collect();
    assert_eq!(listed, [(weak, 1)]);
    assert!(memory.retitle(weak, "Port the AcmeKit parser").await?);
    assert!(!memory.retitle(named, "Something else").await?);
    assert!(memory.retitle(bare, "Rotate the AcmeKit keys").await?);
    assert_eq!(
        stored_title(&pool, weak).await?.as_deref(),
        Some("Port the AcmeKit parser")
    );
    assert_eq!(
        stored_title(&pool, named).await?.as_deref(),
        Some("Named by the hook")
    );
    assert!(!memory.retitle(weak, "A second model title").await?);
    Ok(())
}

fn titled_leaf(conversation: Uuid, content: &str) -> NewSummary {
    NewSummary {
        id: Uuid::new_v4(),
        conversation_id: conversation,
        kind: SummaryKind::Leaf,
        depth: SummaryKind::Leaf.depth(),
        content: content.to_owned(),
        message_count: 0,
        earliest_at: Utc::now(),
        latest_at: Utc::now(),
        model: "fake-chat".to_owned(),
        pipeline_version: 5,
        created_at: Utc::now(),
        sources: Vec::new(),
        child_ids: Vec::new(),
    }
}

async fn seed_untitled(pool: &PgPool, session: &str) -> Result<Uuid, sqlx::Error> {
    let mut tx = deferred(pool).await?;
    let id = sqlx::query_scalar(
        "insert into rh_conversations (session_key, agent, channel, owner_user_id)
         values ($1, 'rivet', 'cli', 'owner') returning id",
    )
    .bind(session)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

async fn stored_title(pool: &PgPool, id: Uuid) -> Result<Option<String>, sqlx::Error> {
    let row = sqlx::query_as::<_, StoredTitle>("select title from rh_conversations where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await?;
    Ok(row.title)
}

async fn seed_conversation(pool: &PgPool, session: &str, title: &str) -> Result<Uuid, sqlx::Error> {
    let mut tx = deferred(pool).await?;
    let id = sqlx::query_scalar(
        "insert into rh_conversations (session_key, agent, channel, title, owner_user_id)
         values ($1, 'rivet', 'cli', $2, 'owner') returning id",
    )
    .bind(session)
    .bind(title)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

async fn seed_message(
    pool: &PgPool,
    conversation: Uuid,
    content: &str,
    created_at: DateTime<Utc>,
) -> Result<Uuid, sqlx::Error> {
    let mut tx = deferred(pool).await?;
    let id = sqlx::query_scalar(
        "insert into rh_messages (conversation_id, agent, channel, role, content, created_at, owner_user_id)
         values ($1, 'rivet', 'cli', 'user', $2, $3, 'owner') returning id",
    )
    .bind(conversation)
    .bind(content)
    .bind(created_at)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

async fn seed_leaf(pool: &PgPool, conversation: Uuid, content: &str) -> Result<(), sqlx::Error> {
    let mut tx = deferred(pool).await?;
    sqlx::query(
        "insert into rh_summaries (conversation_id, content, kind, earliest_at, latest_at, owner_user_id)
         values ($1, $2, 'leaf', clock_timestamp(), clock_timestamp(), 'owner')",
    )
    .bind(conversation)
    .bind(content)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

async fn deferred(
    pool: &PgPool,
) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("select set_config('rivet.defer_embed_enqueue', 'on', true)")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .expect("timestamp")
        .with_timezone(&Utc)
}
