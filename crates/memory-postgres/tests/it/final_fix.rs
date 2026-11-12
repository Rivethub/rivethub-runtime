use kernel::memory::MemoryEntry;
use kernel::memory::{CaptureBatchRequest, CaptureMessage};
use kernel::memory::{MemoryStore, MemoryStoreError};
use memory_postgres::{CaptureOptions, Error, PostgresMemory};
use sqlx::{PgPool, types::Json};
use wire::JsonObject;
use wire::message::Role;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn stamp(text: &str) -> Result<chrono::DateTime<chrono::Utc>, chrono::ParseError> {
    Ok(chrono::DateTime::parse_from_rfc3339(text)?.with_timezone(&chrono::Utc))
}

fn batch(content: String) -> Result<CaptureBatchRequest, chrono::ParseError> {
    Ok(CaptureBatchRequest {
        session_key: "final-fix".to_owned(),
        agent: "agent".to_owned(),
        channel: Some("cli".to_owned()),
        title: None,
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages: vec![CaptureMessage {
            event_id: "event".to_owned(),
            role: Role::User,
            content,
            tool_name: None,
            tool_args: None,
            tool_result: None,
            metadata: None,
            created_at: Some(stamp("2026-10-15T12:00:00Z")?),
        }],
    })
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn missing_channel_refuses_before_any_write_and_supplied_owner_is_preserved(
    pool: PgPool,
) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    let mut batch = batch("hello".to_owned())?;
    batch.channel = None;
    let options = CaptureOptions::owned_by(&testkit::owner()?);
    assert!(matches!(
        store.capture_batch(&batch, &options).await,
        Err(Error::MissingChannel)
    ));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_conversations")
        .fetch_one(&pool)
        .await?;
    assert_eq!(count, 0);
    batch.channel = Some("cli".to_owned());
    store.capture_batch(&batch, &options).await?;
    let owner: (String, String) = sqlx::query_as("SELECT channel, owner_user_id FROM rh_messages")
        .fetch_one(&pool)
        .await?;
    assert_eq!(owner, ("cli".to_owned(), "owner".to_owned()));
    let repeated = store.capture_batch(&batch, &options).await?;
    assert_eq!(repeated.inserted, 0);
    assert_eq!(repeated.skipped, 1);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn capture_caps_scalars_and_merges_provenance_without_losing_metadata(
    pool: PgPool,
) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    let content = format!("{}😀tail", "a".repeat(15_999));
    let mut batch = batch(content)?;
    let message = batch.messages.first_mut().ok_or("message")?;
    message.metadata = Some(serde_json::from_str(
        r#"{"keep":{"n":18446744073709551615},"event_id":"untrusted"}"#,
    )?);
    message.tool_result = Some("😀".repeat(16_001));
    store
        .capture_batch(&batch, &CaptureOptions::owned_by(&testkit::owner()?))
        .await?;
    let (content, tool_result, metadata): (String, String, Json<JsonObject>) =
        sqlx::query_as("SELECT content, tool_result, metadata FROM rh_messages")
            .fetch_one(&pool)
            .await?;
    assert_eq!(content, format!("{}😀", "a".repeat(15_999)));
    assert_eq!(tool_result, "😀".repeat(16_000));
    let expected: JsonObject = serde_json::from_str(
        r#"{"keep":{"n":18446744073709551615},"event_id":"event","truncated":true,"full_content_length":16004,"full_tool_result_length":16001}"#,
    )?;
    assert_eq!(metadata.0, expected);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn capture_drops_nul_bytes_postgres_text_cannot_hold(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    let mut batch = batch("pnpm:1\n\u{7f}ELF\u{2}\u{1}\0\0".to_owned())?;
    let message = batch.messages.first_mut().ok_or("message")?;
    message.tool_result = Some("\0\0head\0".to_owned());
    store
        .capture_batch(&batch, &CaptureOptions::owned_by(&testkit::owner()?))
        .await?;
    let (content, tool_result): (String, String) =
        sqlx::query_as("SELECT content, tool_result FROM rh_messages")
            .fetch_one(&pool)
            .await?;
    assert_eq!(content, "pnpm:1\n\u{7f}ELF\u{2}\u{1}");
    assert_eq!(tool_result, "head");
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn append_rejects_wrong_event_id_types_and_hashes_absent_ids_without_collisions(
    pool: PgPool,
) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    let mut entry = MemoryEntry {
        provenance: None,
        id: None,
        session_id: "session".to_owned(),
        agent: "agent".to_owned(),
        channel: "cli".to_owned(),
        role: Role::User,
        content: "hello".to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: Some(serde_json::from_str(r#"{"event_id":42}"#)?),
        created_at: None,
    };
    assert!(matches!(
        store.append(&testkit::owner()?, vec![entry.clone()]).await,
        Err(MemoryStoreError::InvalidRequest { .. })
    ));
    entry.metadata = None;
    assert_eq!(
        store
            .append(&testkit::owner()?, vec![entry.clone()])
            .await?,
        1
    );
    assert_eq!(
        store
            .append(&testkit::owner()?, vec![entry.clone()])
            .await?,
        0
    );
    entry.tool_name = Some(String::new());
    assert_eq!(store.append(&testkit::owner()?, vec![entry]).await?, 1);
    assert!(matches!(
        store.get_full("invalid uuid").await,
        Err(MemoryStoreError::InvalidRequest { .. })
    ));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn context_without_a_user_query_does_not_run_empty_semantic_search(
    pool: PgPool,
) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    let mut batch = batch("assistant only".to_owned())?;
    batch.messages.first_mut().ok_or("message")?.role = Role::Assistant;
    store
        .capture_batch(&batch, &CaptureOptions::owned_by(&testkit::owner()?))
        .await?;
    let context = store
        .context("final-fix", "2026-10-15T12:01:00Z".parse()?)
        .await?;
    assert!(context.contains("assistant only"));
    assert!(!context.contains("Relevant Context"));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn caller_time_zone_controls_calendar_windows(pool: PgPool) -> TestResult {
    use kernel::memory::{BrowseFilter, Limit, SearchMode, SearchQuery};
    use kernel::memory::{BrowseWindow, MemoryBrowseOrder, MemoryScope};

    let store = PostgresMemory::new(pool.clone());
    let mut batch = batch("release".to_owned())?;
    for (event, at) in [
        ("before", "2026-10-14T18:00:00Z"),
        ("local-day", "2026-10-14T19:00:00Z"),
        ("utc-day", "2026-10-15T01:00:00Z"),
    ] {
        let message = batch.messages.first_mut().ok_or("message")?;
        message.event_id = event.to_owned();
        message.created_at = Some(stamp(at)?);
        store
            .capture_batch(&batch, &CaptureOptions::owned_by(&testkit::owner()?))
            .await?;
    }
    let query = SearchQuery {
        text: "release".to_owned(),
        mode: SearchMode::Regex,
        scope: MemoryScope::Messages,
        agent: None,
        role: None,
        exclude_roles: Vec::new(),
        tool_name: None,
        tag: None,
        limit: Limit::clamped(Some(10), 10, 1..=50),
        since: None,
        before: None,
        window: Some(BrowseWindow::Today),
        expand: Some(false),
        synthesize: Some(false),
    };
    assert!(matches!(
        store.search(query.clone()).await,
        Err(MemoryStoreError::InvalidRequest { .. })
    ));
    let now = "2026-10-15T03:00:00Z".parse()?;
    let zone = chrono::FixedOffset::east_opt(19_800).ok_or("zone")?;
    assert_eq!(
        store
            .search(query.clone().resolve_window(now, &zone))
            .await?
            .results
            .len(),
        2
    );
    assert_eq!(
        store
            .search(query.resolve_window(now, &chrono::Utc))
            .await?
            .results
            .len(),
        1
    );
    let filter = BrowseFilter {
        conversation_id: None,
        agent: None,
        role: None,
        tool_name: None,
        tag: None,
        window: Some(BrowseWindow::Today),
        since: None,
        before: None,
        include_tools: false,
        order: MemoryBrowseOrder::Asc,
        limit: Limit::clamped(Some(10), 10, 1..=50),
    };
    assert!(matches!(
        store.browse(filter.clone()).await,
        Err(MemoryStoreError::InvalidRequest { .. })
    ));
    assert_eq!(
        store.browse(filter.resolve_window(now, &zone)).await?.len(),
        2
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn missing_chunk_schema_is_a_query_error(pool: PgPool) -> TestResult {
    use kernel::memory::MemoryScope;
    use kernel::memory::{Limit, SearchMode, SearchQuery};
    use kernel::model::Embedding;
    use std::sync::Arc;
    use testkit::{FakeProvider, Script};

    let store =
        PostgresMemory::new(pool.clone()).with_query_embed(memory_postgres::QueryEmbed::new(
            Arc::new(FakeProvider::new(vec![Script::Embed(vec![Embedding {
                vector: vec![0.5; 1024],
            }])])),
            None,
            None,
            None,
        ));
    sqlx::query("ALTER TABLE rh_message_chunks RENAME TO unavailable_chunks")
        .execute(&pool)
        .await?;
    let result = store
        .search(SearchQuery {
            text: "release".to_owned(),
            mode: SearchMode::Vector,
            scope: MemoryScope::Messages,
            agent: None,
            role: None,
            exclude_roles: Vec::new(),
            tool_name: None,
            tag: None,
            limit: Limit::clamped(Some(10), 10, 1..=50),
            since: None,
            before: None,
            window: None,
            expand: Some(false),
            synthesize: Some(false),
        })
        .await;
    let Err(MemoryStoreError::Query { source, .. }) = result else {
        return Err("missing chunk schema must propagate".into());
    };
    let source = source
        .downcast_ref::<sqlx::Error>()
        .ok_or("SQLx error source")?;
    assert_eq!(
        source
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("42P01")
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn capture_with_text_postgres_cannot_store_is_a_bad_request(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    let mut batch = batch("write it".to_owned())?;
    let message = batch.messages.first_mut().ok_or("message")?;
    message.tool_name = Some("Write".to_owned());
    message.tool_args = Some(serde_json::from_str(r#"{ "content": "a\u0000b" }"#)?);
    let refused = store
        .capture_batch(&batch, &CaptureOptions::owned_by(&testkit::owner()?))
        .await
        .err()
        .ok_or("the insert went through")?;
    assert!(matches!(
        MemoryStoreError::from(refused),
        MemoryStoreError::InvalidRequest { .. }
    ));
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_messages")
        .fetch_one(&pool)
        .await?;
    assert_eq!(rows, 0);
    Ok(())
}
