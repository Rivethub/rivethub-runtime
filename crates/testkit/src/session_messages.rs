use kernel::memory::BrowseFilter;
use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use kernel::memory::Limit;
use kernel::memory::MemoryBrowseOrder;
use kernel::memory::MemoryStore;
use kernel::memory::MemoryStoreError;
use kernel::memory::MessageCursor;
use kernel::memory::MessagesQuery;
use kernel::memory::SessionCatalog;
use kernel::memory::SummaryStore;
use uuid::Uuid;
use wire::JsonObject;
use wire::message::Role;
use wire::session_id::SessionKey;

use crate::topics::at;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn row(index: u32, role: Role, content: &str) -> CaptureMessage {
    CaptureMessage {
        event_id: format!("m{index}"),
        role,
        content: content.to_owned(),
        tool_name: (role == Role::Tool).then(|| "Bash".to_owned()),
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: Some(at(index)),
    }
}

pub async fn messages_come_in_order_paged_and_counted_like_the_catalog(
    memory: &dyn MemoryStore,
    catalog: &dyn SessionCatalog,
) -> TestResult {
    let key: SessionKey = "claude-code:00000000-0000-4000-8000-0000000000e1".parse()?;
    let contents = [
        "list the files",
        "running ls",
        "Cargo.toml",
        "two entries",
        "thanks",
    ];
    let roles = [
        Role::User,
        Role::Assistant,
        Role::Tool,
        Role::Assistant,
        Role::User,
    ];
    let messages = (0_u32..)
        .zip(roles.into_iter().zip(contents))
        .map(|(index, (role, content))| row(index + 1, role, content))
        .collect();
    memory
        .capture(
            CaptureBatchRequest {
                session_key: key.to_string(),
                agent: "opus".to_owned(),
                channel: Some("cli".to_owned()),
                title: None,
                settings: None,
                task_id: None,
                finalize: None,
                created_at: Some(at(1)),
                updated_at: Some(at(5)),
                messages,
            },
            &crate::owner()?,
            None,
        )
        .await?;
    let mut read = Vec::new();
    let mut after = None;
    loop {
        let page = catalog
            .messages(&key, &MessagesQuery::new(after, Some(2)))
            .await?;
        assert_eq!(page.total, 5, "the total counts every stored row");
        read.extend(page.items);
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    let got: Vec<(&str, Role)> = read
        .iter()
        .map(|entry| (entry.content.as_str(), entry.role))
        .collect();
    let want: Vec<(&str, Role)> = contents.into_iter().zip(roles).collect();
    assert_eq!(got, want, "in order, the tool row included");
    assert_eq!(
        read.get(2).and_then(|entry| entry.tool_name.as_deref()),
        Some("Bash")
    );
    let record = catalog.get(&key).await?.ok_or("the session is listed")?;
    assert_eq!(
        record.message_count, 5,
        "the catalog counts what the route returns"
    );
    let conversation = record
        .conversation
        .ok_or("the record names its conversation")?;
    assert_eq!(catalog.conversation_session(conversation).await?, Some(key));
    Ok(())
}

pub async fn session_and_summary_readers_use_line_order(
    memory: &dyn MemoryStore,
    catalog: &dyn SessionCatalog,
    summaries: &dyn SummaryStore,
) -> TestResult {
    let key: SessionKey = "claude-code:00000000-0000-4000-8000-0000000000e2".parse()?;
    let mut opening = row(3, Role::Assistant, "opening");
    opening.metadata = Some(JsonObject::from_serialize(&serde_json::json!({
        "session_jsonl_line": 1, "uuid": "turn-a"
    }))?);
    let mut later = row(1, Role::User, "later");
    later.metadata = Some(JsonObject::from_serialize(&serde_json::json!({
        "session_jsonl_line": u64::MAX
    }))?);
    let mut tool = row(4, Role::Tool, "tool");
    tool.metadata = Some(JsonObject::from_serialize(&serde_json::json!({
        "assistant_uuid": "turn-a"
    }))?);
    memory
        .capture(
            CaptureBatchRequest {
                session_key: key.to_string(),
                agent: "opus".to_owned(),
                channel: Some("cli".to_owned()),
                title: None,
                settings: None,
                task_id: None,
                finalize: None,
                created_at: Some(at(1)),
                updated_at: Some(at(4)),
                messages: vec![later, row(2, Role::User, "fallback"), opening, tool],
            },
            &crate::owner()?,
            None,
        )
        .await?;

    let first = catalog
        .messages(&key, &MessagesQuery::new(None, Some(2)))
        .await?;
    assert_eq!(first.total, 4);
    assert_eq!(
        first
            .items
            .iter()
            .map(|entry| entry.content.as_str())
            .collect::<Vec<_>>(),
        ["fallback", "opening"]
    );
    let cursor = kernel::memory::MessageCursor::decode(&first.next.ok_or("next page")?.encode())?;
    let second = catalog
        .messages(&key, &MessagesQuery::new(Some(cursor), Some(2)))
        .await?;
    assert!(second.next.is_none());
    assert_eq!(
        second
            .items
            .iter()
            .map(|entry| entry.content.as_str())
            .collect::<Vec<_>>(),
        ["tool", "later"]
    );

    let conversation = catalog
        .get(&key)
        .await?
        .and_then(|row| row.conversation)
        .ok_or("conversation ID")?;
    let window = summaries
        .conversation(conversation)
        .await?
        .ok_or("summary window")?;
    assert_eq!(
        window
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        ["fallback", "opening", "tool", "later"]
    );
    Ok(())
}

pub async fn capture_session_fixture(
    memory: &dyn MemoryStore,
    key: &SessionKey,
    messages: Vec<CaptureMessage>,
) -> TestResult {
    memory
        .capture(
            CaptureBatchRequest {
                session_key: key.to_string(),
                agent: "opus".to_owned(),
                channel: Some("cli".to_owned()),
                title: None,
                settings: None,
                task_id: None,
                finalize: None,
                created_at: Some(at(1)),
                updated_at: Some(at(1)),
                messages,
            },
            &crate::owner()?,
            None,
        )
        .await?;
    Ok(())
}

fn positioned(index: u32, path: &str, line: u64, content: &str) -> TestResult<CaptureMessage> {
    let mut message = row(index, Role::User, content);
    message.metadata = Some(JsonObject::from_serialize(&serde_json::json!({
        "session_jsonl_path": path, "session_jsonl_line": line,
    }))?);
    Ok(message)
}

async fn first_cursor(catalog: &dyn SessionCatalog, key: &SessionKey) -> TestResult<MessageCursor> {
    let page = catalog
        .messages(key, &MessagesQuery::new(None, Some(1)))
        .await?;
    let cursor = page.next.ok_or("next page")?;
    Ok(MessageCursor::decode(&cursor.encode())?)
}

async fn assert_cursor_error(
    catalog: &dyn SessionCatalog,
    key: &SessionKey,
    cursor: MessageCursor,
) {
    let result = catalog
        .messages(key, &MessagesQuery::new(Some(cursor), None))
        .await;
    assert!(
        matches!(result, Err(MemoryStoreError::InvalidRequest { message })
        if message.contains("is not one this server issued"))
    );
}

pub async fn inserting_before_the_cursor_requires_restarting_the_session(
    memory: &dyn MemoryStore,
    catalog: &dyn SessionCatalog,
) -> TestResult {
    let key: SessionKey = "claude-code:00000000-0000-4000-8000-0000000000e3".parse()?;
    capture_session_fixture(
        memory,
        &key,
        vec![
            positioned(40, "b", 1, "first")?,
            positioned(100, "a", 1, "second")?,
            positioned(110, "a", 2, "third")?,
        ],
    )
    .await?;
    let first = first_cursor(catalog, &key).await?;
    let legacy = MessageCursor::decode(&format!(
        "{}.{}",
        first.created_at.timestamp_micros(),
        first.id
    ))?;
    assert_cursor_error(catalog, &key, legacy).await;
    let second = catalog
        .messages(&key, &MessagesQuery::new(None, Some(2)))
        .await?
        .next
        .ok_or("next page")?;
    capture_session_fixture(memory, &key, vec![positioned(500, "b", 0, "insert")?]).await?;
    assert_cursor_error(catalog, &key, first).await;
    assert_cursor_error(catalog, &key, second).await;
    Ok(())
}

pub async fn deleting_messages_preserves_only_unchanged_positioned_prefixes(
    memory: &dyn MemoryStore,
    catalog: &dyn SessionCatalog,
    delete: impl AsyncFn(Uuid) -> TestResult,
) -> TestResult {
    let key: SessionKey = "claude-code:00000000-0000-4000-8000-0000000000e4".parse()?;
    capture_session_fixture(
        memory,
        &key,
        vec![
            positioned(100, "a", 1, "cursor")?,
            positioned(10, "a", 2, "second")?,
            positioned(20, "a", 3, "third")?,
            positioned(30, "a", 4, "fourth")?,
        ],
    )
    .await?;
    let first = first_cursor(catalog, &key).await?;
    let second = catalog
        .messages(&key, &MessagesQuery::new(None, Some(2)))
        .await?
        .next
        .ok_or("next page")?;
    let third = catalog
        .messages(&key, &MessagesQuery::new(None, Some(3)))
        .await?
        .next
        .ok_or("next page")?;
    delete(third.id).await?;
    let tail = catalog
        .messages(&key, &MessagesQuery::new(Some(second), None))
        .await?;
    assert_eq!(tail.items.len(), 1);
    assert_eq!(tail.items.first().ok_or("last message")?.content, "fourth");
    delete(first.id).await?;
    assert_cursor_error(catalog, &key, first).await;
    assert_cursor_error(catalog, &key, second).await;
    Ok(())
}

pub async fn deleted_unpositioned_cursor_uses_the_timestamp_keyset(
    memory: &dyn MemoryStore,
    catalog: &dyn SessionCatalog,
    delete: impl AsyncFn(Uuid) -> TestResult,
) -> TestResult {
    let key: SessionKey = "claude-code:00000000-0000-4000-8000-0000000000e5".parse()?;
    capture_session_fixture(
        memory,
        &key,
        vec![row(10, Role::User, "first"), row(20, Role::User, "second")],
    )
    .await?;
    let first = first_cursor(catalog, &key).await?;
    delete(first.id).await?;
    let page = catalog
        .messages(&key, &MessagesQuery::new(Some(first), None))
        .await?;
    assert_eq!(page.total, 1);
    assert_eq!(
        page.items.first().ok_or("remaining message")?.content,
        "second"
    );
    Ok(())
}

pub async fn scoped_browse_keeps_timestamp_window_order(
    memory: &dyn MemoryStore,
    catalog: &dyn SessionCatalog,
) -> TestResult {
    let key: SessionKey = "claude-code:00000000-0000-4000-8000-0000000000e6".parse()?;
    capture_session_fixture(
        memory,
        &key,
        vec![
            positioned(10, "a", 1, "first")?,
            positioned(30, "a", 2, "last by time")?,
            positioned(20, "a", 3, "middle by time")?,
        ],
    )
    .await?;
    let conversation = catalog
        .get(&key)
        .await?
        .and_then(|row| row.conversation)
        .ok_or("conversation ID")?;
    let mut filter = BrowseFilter {
        conversation_id: Some(conversation),
        agent: None,
        role: None,
        tool_name: None,
        tag: None,
        window: None,
        since: None,
        before: None,
        include_tools: true,
        order: MemoryBrowseOrder::Asc,
        limit: Limit::clamped(Some(2), 50, 1..=100),
    };
    let first = memory.browse(filter.clone()).await?;
    assert_eq!(contents(&first), ["first", "middle by time"]);
    filter.since = first.last().and_then(|entry| entry.created_at);
    assert_eq!(
        contents(&memory.browse(filter.clone()).await?),
        ["middle by time", "last by time"]
    );
    filter.order = MemoryBrowseOrder::Desc;
    filter.since = None;
    filter.before = Some(at(30));
    assert_eq!(
        contents(&memory.browse(filter).await?),
        ["middle by time", "first"]
    );
    Ok(())
}

fn contents(entries: &[kernel::memory::MemoryEntry]) -> Vec<&str> {
    entries.iter().map(|entry| entry.content.as_str()).collect()
}

pub async fn session_and_summary_readers_use_hook_coordinates(
    memory: &dyn MemoryStore,
    catalog: &dyn SessionCatalog,
    summaries: &dyn SummaryStore,
) -> TestResult {
    let cases = [
        (
            "opencode:synthetic-hook-order",
            [
                serde_json::json!({"session_store_position": 0, "session_sqlite_path": "opencode.db", "store_format": "opencode-part"}),
                serde_json::json!({"session_store_position": u64::MAX, "session_sqlite_path": "opencode.db", "store_format": "opencode-part"}),
            ],
        ),
        (
            "claude-code:00000000-0000-4000-8000-0000000000f2",
            [
                serde_json::json!({"hook_sequence": 1, "hook_producer": "producer"}),
                serde_json::json!({"hook_sequence": u64::MAX, "hook_producer": "producer"}),
            ],
        ),
    ];
    for (key, [first_meta, second_meta]) in cases {
        let key = key.parse()?;
        let mut first = row(2, Role::User, "observed first");
        first.metadata = Some(JsonObject::from_serialize(&first_meta)?);
        let mut second = row(1, Role::User, "observed second");
        second.metadata = Some(JsonObject::from_serialize(&second_meta)?);
        capture_session_fixture(memory, &key, vec![second, first]).await?;

        let head = catalog
            .messages(&key, &MessagesQuery::new(None, Some(1)))
            .await?;
        let tail = catalog
            .messages(&key, &MessagesQuery::new(head.next, Some(1)))
            .await?;
        let panel: Vec<_> = head
            .items
            .into_iter()
            .chain(tail.items)
            .map(|row| row.content)
            .collect();
        let conversation = catalog
            .get(&key)
            .await?
            .and_then(|row| row.conversation)
            .ok_or("conversation ID")?;
        let summary = summaries
            .conversation(conversation)
            .await?
            .ok_or("summary window")?;
        let summary: Vec<_> = summary
            .messages
            .into_iter()
            .map(|message| message.content)
            .collect();
        assert_eq!(panel, ["observed first", "observed second"]);
        assert_eq!(summary, panel);
    }
    Ok(())
}

pub async fn a_tool_replay_fills_position_and_invalidates_an_earlier_cursor(
    memory: &dyn MemoryStore,
    catalog: &dyn SessionCatalog,
    summaries: &dyn SummaryStore,
) -> TestResult {
    let key: SessionKey = "claude-code:41414141-4141-4141-4141-414141414141".parse()?;
    let (messages, batch) = tool_replay(&key)?;
    capture_session_fixture(memory, &key, messages).await?;
    let first = catalog
        .messages(&key, &MessagesQuery::new(None, Some(3)))
        .await?;
    assert_eq!(
        first
            .items
            .last()
            .ok_or("last message on first page")?
            .content,
        "The key is in the drawer"
    );
    let cursor = MessageCursor::decode(&first.next.ok_or("next page")?.encode())?;
    let old_tool = catalog
        .messages(&key, &MessagesQuery::new(Some(cursor), None))
        .await?
        .items
        .into_iter()
        .next()
        .ok_or("early tool")?;

    let owner = crate::owner()?;
    let result = memory.capture(batch.clone(), &owner, None).await?;
    assert_eq!((result.inserted, result.skipped), (0, 1));
    assert_cursor_error(catalog, &key, cursor).await;

    let query = MessagesQuery::new(None, None);
    let fixed = catalog.messages(&key, &query).await?;
    assert_eq!(
        contents(&fixed.items),
        [
            "Find the cabin key",
            "Looking in the drawer",
            "[tool call] Read",
            "The key is in the drawer"
        ]
    );
    let tool = fixed.items.get(2).ok_or("positioned tool")?;
    let mut expected = old_tool;
    let metadata = expected.metadata.as_mut().ok_or("original tool metadata")?;
    metadata.insert("session_jsonl_line", 2)?;
    metadata.insert("ordinal", 1)?;
    metadata.insert("assistant_uuid", "answer")?;
    assert_eq!(*tool, expected);

    let conversation = Uuid::parse_str(&result.conversation_id)?;
    let window = summaries
        .conversation(conversation)
        .await?
        .ok_or("summary window")?;
    let summary: Vec<_> = window
        .messages
        .iter()
        .map(|row| row.content.as_str())
        .collect();
    assert_eq!(summary, contents(&fixed.items));

    let result = memory.capture(batch, &owner, None).await?;
    assert_eq!((result.inserted, result.skipped), (0, 1));
    assert_eq!(catalog.messages(&key, &query).await?, fixed);
    Ok(())
}

fn tool_replay(key: &SessionKey) -> TestResult<(Vec<CaptureMessage>, CaptureBatchRequest)> {
    let path = "/tmp/synthetic-session.jsonl";
    let mut live = row(50, Role::Tool, "[tool call] Read");
    live.event_id = format!("{key}:tool:toolu_synthetic_read");
    live.tool_name = Some("Read".to_owned());
    live.tool_args = Some(serde_json::from_str(r#"{"path":"drawer"}"#)?);
    live.tool_result = Some("Found it live".to_owned());
    live.metadata = Some(JsonObject::from_serialize(&serde_json::json!({
        "session_jsonl_path": path,
        "tool_use_id": "toolu_synthetic_read",
        "hook_producer": "producer-a",
        "hook_sequence": 1,
        "source": "claude-code-hook",
        "native_event_id": null
    }))?);
    let mut opening = positioned(20, path, 1, "Looking in the drawer")?;
    opening.role = Role::Assistant;
    let mut closing = positioned(10, path, 3, "The key is in the drawer")?;
    closing.role = Role::Assistant;
    let mut replay = row(19, Role::Tool, "Replay content");
    replay.event_id.clone_from(&live.event_id);
    replay.tool_args = Some(serde_json::from_str(r#"{"path":"different"}"#)?);
    replay.tool_result = Some("Replay result".to_owned());
    replay.metadata = Some(JsonObject::from_serialize(&serde_json::json!({
        "session_jsonl_path": "/tmp/different-session.jsonl",
        "session_jsonl_line": 2,
        "ordinal": 1,
        "tool_use_id": "conflicting-id",
        "assistant_uuid": "answer",
        "native_event_id": "does not replace stored null",
        "source": "claude-code-transcript",
        "unrelated": true
    }))?);
    let batch = CaptureBatchRequest {
        session_key: key.to_string(),
        agent: "opus".to_owned(),
        channel: Some("cli".to_owned()),
        title: None,
        settings: None,
        task_id: None,
        finalize: None,
        created_at: Some(at(1)),
        updated_at: Some(at(50)),
        messages: vec![replay],
    };

    Ok((
        vec![
            live,
            positioned(30, path, 0, "Find the cabin key")?,
            opening,
            closing,
        ],
        batch,
    ))
}
