use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use kernel::memory::MemoryStore;
use kernel::memory::MessagesQuery;
use kernel::memory::SessionCatalog;
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
