use kernel::memory::BrowseFilter;
use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use kernel::memory::Limit;
use kernel::memory::MemoryBrowseOrder;
use kernel::memory::MemoryStore;
use kernel::memory::MessagesQuery;
use kernel::memory::SessionCatalog;
use kernel::memory::SummaryStore;
use uuid::Uuid;
use wire::JsonObject;
use wire::message::Role;
use wire::session_id::SessionKey;

use crate::topics::at;

type TestResult = Result<(), Box<dyn std::error::Error>>;

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

pub async fn readers_use_line_order_before_page_and_browse_limits(
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
        "session_jsonl_line": 2
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
    assert_browse_order(memory, conversation).await
}

async fn assert_browse_order(memory: &dyn MemoryStore, conversation: Uuid) -> TestResult {
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
    for (order, expected) in [
        (MemoryBrowseOrder::Asc, ["fallback", "opening"]),
        (MemoryBrowseOrder::Desc, ["later", "tool"]),
    ] {
        filter.order = order;
        let entries = memory.browse(filter.clone()).await?;
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.content.as_str())
                .collect::<Vec<_>>(),
            expected
        );
    }
    Ok(())
}
