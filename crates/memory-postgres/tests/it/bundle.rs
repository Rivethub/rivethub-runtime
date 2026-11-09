use crate::common;

use chrono::DateTime;
use chrono::Utc;
use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use kernel::memory::SessionDeletion as _;
use kernel::owner::Owner;
use memory_postgres::CaptureOptions;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use uuid::Uuid;
use wire::JsonObject;
use wire::capture::BundleAuthor;
use wire::message::Role;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn author(user: &str) -> BundleAuthor {
    BundleAuthor {
        user: user.to_owned(),
        node: "desk".to_owned(),
    }
}

fn at(text: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
    Ok(DateTime::parse_from_rfc3339(text)?.with_timezone(&Utc))
}

fn object(text: &str) -> Result<JsonObject, serde_json::Error> {
    serde_json::from_str(text)
}

fn said(event_id: &str, content: &str, created_at: DateTime<Utc>) -> CaptureMessage {
    CaptureMessage {
        event_id: event_id.to_owned(),
        role: Role::User,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: Some(created_at),
    }
}

fn batch(session_key: &str, messages: Vec<CaptureMessage>) -> CaptureBatchRequest {
    CaptureBatchRequest {
        session_key: session_key.to_owned(),
        agent: "rivet".to_owned(),
        channel: Some("claude-code".to_owned()),
        title: Some(format!("title of {session_key}")),
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages,
    }
}

async fn captured(
    store: &PostgresMemory,
    owner: &str,
    batch: &CaptureBatchRequest,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let owner = Owner::new(owner.to_owned())?;
    let result = store
        .capture_batch(batch, &CaptureOptions::owned_by(&owner))
        .await?;
    Ok(Uuid::parse_str(&result.conversation_id)?)
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn export_selects_only_named_sessions_in_message_order(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let store = PostgresMemory::new(pool.clone());
    let mut tool = said("evt-tool", "[tool call] Read", at("2026-10-14T09:05:00Z")?);
    tool.role = Role::Tool;
    tool.tool_name = Some("Read".to_owned());
    tool.tool_args = Some(object(r#"{"file_path":"/work/a.rs"}"#)?);
    tool.tool_result = Some("fn main() {}\n\twith a tab and \"quotes\"".to_owned());
    tool.metadata = Some(object(r#"{"uuid":"u-1"}"#)?);
    let mut first = batch(
        "claude-code:first",
        vec![
            said("evt-late", "said last", at("2026-10-14T09:10:00Z")?),
            said("evt-early", "said first", at("2026-10-14T09:00:00Z")?),
            tool.clone(),
        ],
    );
    first.settings = Some(object(r#"{"model":"opus"}"#)?);
    let first_id = captured(&store, "phil", &first).await?;
    let skipped = batch(
        "claude-code:skipped",
        vec![said(
            "evt-skip",
            "not exported",
            at("2026-10-14T09:00:00Z")?,
        )],
    );
    captured(&store, "phil", &skipped).await?;
    let third = batch(
        "codex:third",
        vec![said("evt-third", "third", at("2026-10-14T10:00:00Z")?)],
    );
    let third_id = captured(&store, "phil", &third).await?;

    let sessions = store
        .bundled_sessions(&author("phil"), Some(&[third_id, first_id]))
        .await?;

    let mut ids: Vec<_> = sessions
        .iter()
        .map(|session| session.origin.conversation_id.clone())
        .collect();
    ids.sort();
    let mut wanted = vec![first_id.to_string(), third_id.to_string()];
    wanted.sort();
    assert_eq!(ids, wanted);
    let exported = sessions
        .iter()
        .find(|session| session.origin.conversation_id == first_id.to_string())
        .ok_or("first session")?;
    assert_eq!(exported.origin.author, "phil");
    assert_eq!(exported.origin.session_key, "claude-code:first");
    assert_eq!(exported.batch.settings, first.settings);
    assert_eq!(
        exported.batch.title.as_deref(),
        Some("title of claude-code:first")
    );
    assert_eq!(
        exported
            .batch
            .messages
            .iter()
            .map(|message| message.event_id.as_str())
            .collect::<Vec<_>>(),
        ["evt-early", "evt-tool", "evt-late"]
    );
    let read = exported
        .batch
        .messages
        .iter()
        .find(|message| message.event_id == "evt-tool")
        .ok_or("tool message")?;
    assert_eq!(read.tool_name, tool.tool_name);
    assert_eq!(read.tool_args, tool.tool_args);
    assert_eq!(read.tool_result, tool.tool_result);
    assert_eq!(read.metadata, tool.metadata);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn export_never_returns_another_owners_session(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let store = PostgresMemory::new(pool.clone());
    let phils = captured(
        &store,
        "phil",
        &batch(
            "claude-code:phil",
            vec![said("evt-p", "mine", at("2026-10-14T09:00:00Z")?)],
        ),
    )
    .await?;
    let alexs = captured(
        &store,
        "alex",
        &batch(
            "claude-code:alex",
            vec![said("evt-a", "not phil's", at("2026-10-14T09:00:00Z")?)],
        ),
    )
    .await?;

    let named = store
        .bundled_sessions(&author("phil"), Some(&[alexs, phils]))
        .await?;
    let everything = store.bundled_sessions(&author("phil"), None).await?;

    for sessions in [&named, &everything] {
        let ids: Vec<_> = sessions
            .iter()
            .map(|session| session.origin.conversation_id.clone())
            .collect();
        assert_eq!(ids, [phils.to_string()]);
    }
    assert!(!serde_json::to_string(&everything)?.contains("not phil's"));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_trashed_session_is_exported_only_when_named(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let store = PostgresMemory::new(pool.clone());
    let kept = captured(
        &store,
        "phil",
        &batch(
            "claude-code:kept",
            vec![said("evt-k", "kept", at("2026-10-14T09:00:00Z")?)],
        ),
    )
    .await?;
    let trashed = captured(
        &store,
        "phil",
        &batch(
            "claude-code:trashed",
            vec![said("evt-t", "trashed", at("2026-10-14T09:01:00Z")?)],
        ),
    )
    .await?;
    store
        .trash_session("claude-code:trashed", None, at("2026-10-15T00:00:00Z")?)
        .await?;

    let everything = store.bundled_sessions(&author("phil"), None).await?;
    let named = store
        .bundled_sessions(&author("phil"), Some(&[trashed]))
        .await?;

    let ids = |sessions: &[wire::capture::BundledSession]| -> Vec<String> {
        sessions
            .iter()
            .map(|session| session.origin.conversation_id.clone())
            .collect()
    };
    assert_eq!(ids(&everything), [kept.to_string()]);
    assert_eq!(ids(&named), [trashed.to_string()]);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn export_keeps_capture_order_among_messages_captured_without_a_timestamp(
    pool: PgPool,
) -> TestResult {
    common::install_graphile(&pool).await?;
    let store = PostgresMemory::new(pool.clone());
    let tied = at("2026-10-14T09:00:00Z")?;
    let steps: Vec<String> = (1..=8).map(|step| format!("step {step}")).collect();
    let messages = steps
        .iter()
        .map(|step| CaptureMessage {
            created_at: None,
            ..said(&format!("evt-{step}"), step, tied)
        })
        .collect();
    let id = captured(&store, "phil", &batch("claude-code:undated", messages)).await?;

    let sessions = store.bundled_sessions(&author("phil"), Some(&[id])).await?;

    let session = sessions.first().ok_or("undated session")?;
    let exported: Vec<&str> = session
        .batch
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect();
    assert_eq!(exported, steps);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_batch_that_waits_on_the_session_lock_sorts_after_the_batch_holding_it(
    pool: PgPool,
) -> TestResult {
    common::install_graphile(&pool).await?;
    let store = PostgresMemory::new(pool.clone());
    let key = "claude-code:overlap";
    let undated = |content: &str| CaptureMessage {
        created_at: None,
        ..said(&format!("evt-{content}"), content, Utc::now())
    };
    let id = captured(&store, "phil", &batch(key, vec![undated("seed")])).await?;
    let warm = (pool.acquire().await?, pool.acquire().await?);
    drop(warm);

    let first: Vec<String> = (0..3_000).map(|n| format!("a {n}")).collect();
    let metadata = object("{}")?;
    let mut holder = memory_postgres::CaptureWriter::begin(&pool, key).await?;
    let second = tokio::spawn({
        let store = PostgresMemory::new(pool.clone());
        let request = batch(key, vec![undated("b")]);
        async move {
            captured(&store, "phil", &request)
                .await
                .map_err(|e| e.to_string())
        }
    });
    while sqlx::query_scalar::<_, i64>(
        "select count(*) from pg_locks where locktype = 'advisory' and not granted",
    )
    .fetch_one(&pool)
    .await?
        == 0
    {
        tokio::task::yield_now().await;
    }
    for (index, content) in first.iter().enumerate() {
        holder
            .insert_message(&memory_postgres::MessageRow {
                conversation_id: id,
                agent: "rivet",
                channel: "claude-code",
                role: Role::User,
                content,
                tool_name: None,
                tool_args: None,
                tool_result: None,
                metadata: &metadata,
                owner_user_id: "phil",
                created_at: None,
                batch_index: i64::try_from(index)?,
            })
            .await?;
    }
    holder.commit().await?;
    second.await??;

    let sessions = store.bundled_sessions(&author("phil"), Some(&[id])).await?;

    let session = sessions.first().ok_or("overlap session")?;
    let exported: Vec<&str> = session
        .batch
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect();
    let expected: Vec<&str> = std::iter::once("seed")
        .chain(first.iter().map(String::as_str))
        .chain(std::iter::once("b"))
        .collect();
    assert_eq!(exported.len(), expected.len());
    assert_eq!(
        exported.iter().position(|c| *c == "b"),
        Some(expected.len() - 1)
    );
    assert_eq!(exported, expected);
    Ok(())
}
