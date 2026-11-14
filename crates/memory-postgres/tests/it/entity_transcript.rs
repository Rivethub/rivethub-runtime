#![cfg(test)]

use chrono::DateTime;
use chrono::Utc;
use kernel::wiki::entity_pass::EntityLeaf;
use kernel::wiki::entity_pass::EntityStore;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn entity_transcript_prefers_sources_and_falls_back_to_the_window(
    pool: PgPool,
) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    let conversation = seed_conversation(&pool, "claude-code:entity-transcript").await?;
    let other = seed_conversation(&pool, "claude-code:entity-other").await?;
    let outside = seed_message(
        &pool,
        conversation,
        note("user", "outside", "2026-10-10T11:00:00Z"),
    )
    .await?;
    let early = seed_message(
        &pool,
        conversation,
        note(
            "user",
            "in the window but not a source",
            "2026-10-10T12:00:00Z",
        ),
    )
    .await?;
    let sourced_late = seed_message(
        &pool,
        conversation,
        note("assistant", "sourced later", "2026-10-10T12:05:00Z"),
    )
    .await?;
    let sourced_early = seed_message(
        &pool,
        conversation,
        note("user", "sourced earlier", "2026-10-10T12:02:00Z"),
    )
    .await?;
    let window_start = seed_message(
        &pool,
        conversation,
        note("user", "window start", "2026-10-10T13:00:00Z"),
    )
    .await?;
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            untyped,
            reason = "tool arguments and model JSON are arbitrary objects"
        )
    )]
    let window_end = seed_message(
        &pool,
        conversation,
        Message {
            role: "tool",
            content: "Bash",
            tool_name: Some("Bash"),
            tool_args: Some(serde_json::json!({"command": "hostname"})),
            tool_result: Some("gpubox\n"),
            created_at: at("2026-10-10T13:05:00Z"),
        },
    )
    .await?;
    let past_window = seed_message(
        &pool,
        conversation,
        note("user", "past the window", "2026-10-10T13:06:00Z"),
    )
    .await?;
    let foreign = seed_message(
        &pool,
        other,
        note("user", "other conversation", "2026-10-10T13:01:00Z"),
    )
    .await?;
    let sourced = seed_leaf(
        &pool,
        conversation,
        "sourced leaf",
        at("2026-10-10T12:00:00Z"),
        at("2026-10-10T12:05:00Z"),
    )
    .await?;
    let window = seed_leaf(
        &pool,
        conversation,
        "window leaf",
        at("2026-10-10T13:00:00Z"),
        at("2026-10-10T13:05:00Z"),
    )
    .await?;
    link(&pool, sourced, sourced_late, 0).await?;
    link(&pool, sourced, sourced_early, 1).await?;

    let rows = memory
        .transcript(&[
            leaf(
                sourced,
                conversation,
                "2026-10-10T12:00:00Z",
                "2026-10-10T12:05:00Z",
            ),
            leaf(
                window,
                conversation,
                "2026-10-10T13:00:00Z",
                "2026-10-10T13:05:00Z",
            ),
        ])
        .await?;
    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    assert_eq!(
        ids,
        vec![sourced_early, sourced_late, window_start, window_end]
    );
    assert!(!ids.contains(&outside));
    assert!(!ids.contains(&early));
    assert!(!ids.contains(&past_window));
    assert!(!ids.contains(&foreign));
    let tool = rows.last().ok_or("tool row")?;
    assert_eq!(tool.tool_name.as_deref(), Some("Bash"));
    assert_eq!(
        tool.tool_args
            .as_ref()
            .and_then(|value| value.get("command"))
            .and_then(|value| value.as_str()),
        Some("hostname")
    );
    assert_eq!(tool.tool_result.as_deref(), Some("gpubox\n"));
    assert_eq!(tool.at, at("2026-10-10T13:05:00Z"));
    Ok(())
}

fn leaf(id: Uuid, conversation: Uuid, start: &str, end: &str) -> EntityLeaf {
    EntityLeaf {
        id,
        conversation_id: Some(conversation),
        content: "leaf".to_owned(),
        start: at(start),
        end: at(end),
        owner: Some("owner".to_owned()),
    }
}

async fn seed_conversation(pool: &PgPool, session: &str) -> Result<Uuid, sqlx::Error> {
    let mut tx = deferred(pool).await?;
    let id = sqlx::query_scalar(
        "insert into rh_conversations (session_key, agent, channel, title, owner_user_id)
         values ($1, 'rivet', 'cli', $1, 'owner') returning id",
    )
    .bind(session)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        untyped,
        reason = "tool arguments and model JSON are arbitrary objects"
    )
)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
    tool_name: Option<&'a str>,
    tool_args: Option<serde_json::Value>,
    tool_result: Option<&'a str>,
    created_at: DateTime<Utc>,
}

fn note<'a>(role: &'a str, content: &'a str, created_at: &str) -> Message<'a> {
    Message {
        role,
        content,
        tool_name: None,
        tool_args: None,
        tool_result: None,
        created_at: at(created_at),
    }
}

async fn seed_message(
    pool: &PgPool,
    conversation: Uuid,
    message: Message<'_>,
) -> Result<Uuid, sqlx::Error> {
    let mut tx = deferred(pool).await?;
    let id = sqlx::query_scalar(
        "insert into rh_messages
            (conversation_id, agent, channel, role, content, tool_name, tool_args, tool_result, created_at, owner_user_id)
         values ($1, 'rivet', 'cli', $2, $3, $4, $5, $6, $7, 'owner') returning id",
    )
    .bind(conversation)
    .bind(message.role)
    .bind(message.content)
    .bind(message.tool_name)
    .bind(message.tool_args)
    .bind(message.tool_result)
    .bind(message.created_at)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

async fn seed_leaf(
    pool: &PgPool,
    conversation: Uuid,
    content: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Uuid, sqlx::Error> {
    let mut tx = deferred(pool).await?;
    let id = sqlx::query_scalar(
        "insert into rh_summaries
            (conversation_id, content, kind, earliest_at, latest_at, owner_user_id)
         values ($1, $2, 'leaf', $3, $4, 'owner') returning id",
    )
    .bind(conversation)
    .bind(content)
    .bind(start)
    .bind(end)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

async fn link(
    pool: &PgPool,
    summary: Uuid,
    message: Uuid,
    ordinal: i32,
) -> Result<(), sqlx::Error> {
    let mut tx = deferred(pool).await?;
    sqlx::query(
        "insert into rh_summary_sources (summary_id, message_id, ordinal) values ($1, $2, $3)",
    )
    .bind(summary)
    .bind(message)
    .bind(ordinal)
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
    DateTime::parse_from_rfc3339(text).map_or(DateTime::UNIX_EPOCH, |time| time.with_timezone(&Utc))
}
