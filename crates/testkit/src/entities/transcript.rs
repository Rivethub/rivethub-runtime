use kernel::memory::{BrowseFilter, Limit, MemoryBrowseOrder, MemoryEntry, SummarySource};
use serde::Serialize;
use uuid::Uuid;
use wire::{JsonObject, message::Role};

use super::{EntityScenarioStore, ScenarioResult, at, leaf_with_sources};
use crate::topics::Outcome;

#[derive(Serialize)]
struct Position {
    session_jsonl_line: u32,
}

#[derive(Serialize)]
struct Tool {
    command: &'static str,
}

fn entry(key: &str, role: Role, text: &str, time: &str) -> ScenarioResult<MemoryEntry> {
    let id = Uuid::new_v4();
    Ok(MemoryEntry {
        id: Some(id.to_string()),
        session_id: key.to_owned(),
        agent: "rivet".to_owned(),
        channel: "claude-code".to_owned(),
        role,
        content: text.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: Some(at(time)?),
        provenance: None,
    })
}

async fn messages(store: &impl EntityScenarioStore) -> ScenarioResult<(Uuid, [Uuid; 4])> {
    let key = format!("claude-code:{}", Uuid::new_v4());
    let conversation =
        crate::filing::capture(store, &key, "claude-code", "Transcript scenario", None).await?;
    let outside = entry(&key, Role::User, "outside", "2026-10-10T11:00:00Z")?;
    let unlinked = entry(
        &key,
        Role::User,
        "in window, not linked",
        "2026-10-10T12:00:00Z",
    )?;
    let mut late = entry(
        &key,
        Role::Assistant,
        "linked later",
        "2026-10-10T12:05:00Z",
    )?;
    let mut early = entry(&key, Role::User, "linked earlier", "2026-10-10T12:02:00Z")?;
    late.metadata = Some(JsonObject::from_serialize(&Position {
        session_jsonl_line: 1,
    })?);
    early.metadata = Some(JsonObject::from_serialize(&Position {
        session_jsonl_line: 2,
    })?);
    let start = entry(&key, Role::User, "window start", "2026-10-10T13:00:00Z")?;
    let mut tool = entry(&key, Role::Tool, "Bash", "2026-10-10T13:05:00Z")?;
    tool.tool_name = Some("Bash".to_owned());
    tool.tool_args = Some(JsonObject::from_serialize(&Tool {
        command: "hostname",
    })?);
    tool.tool_result = Some("tinbox\n".to_owned());
    let past = entry(&key, Role::User, "past the window", "2026-10-10T13:06:00Z")?;
    let other = format!("claude-code:{}", Uuid::new_v4());
    let foreign = entry(
        &other,
        Role::User,
        "other conversation",
        "2026-10-10T13:01:00Z",
    )?;
    store
        .append(
            &crate::owner()?,
            vec![outside, unlinked, late, early, start, tool, past, foreign],
        )
        .await?;
    Ok((conversation, message_ids(store, conversation).await?))
}

async fn message_ids(
    store: &impl EntityScenarioStore,
    conversation: Uuid,
) -> ScenarioResult<[Uuid; 4]> {
    let saved = store
        .browse(BrowseFilter {
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
            limit: Limit::clamped(None, 30, 1..=100),
        })
        .await?;
    let id = |text| -> ScenarioResult<Uuid> {
        let found = saved
            .iter()
            .find(|row| row.content == text)
            .and_then(|row| row.id.as_deref())
            .ok_or("captured message missing")?;
        Ok(Uuid::parse_str(found)?)
    };
    Ok([
        id("linked later")?,
        id("linked earlier")?,
        id("window start")?,
        id("Bash")?,
    ])
}

pub async fn sources_then_window(store: &impl EntityScenarioStore) -> Outcome {
    let (conversation, [late, early, start, tool]) = messages(store).await?;
    let sourced = leaf_with_sources(
        store,
        conversation,
        "sourced leaf",
        at("2026-10-10T12:00:00Z")?,
        at("2026-10-10T12:05:00Z")?,
        vec![
            SummarySource {
                message_id: late,
                ordinal: 0,
            },
            SummarySource {
                message_id: early,
                ordinal: 1,
            },
        ],
    )
    .await?;
    let window = leaf_with_sources(
        store,
        conversation,
        "unlinked leaf",
        at("2026-10-10T13:00:00Z")?,
        at("2026-10-10T13:05:00Z")?,
        Vec::new(),
    )
    .await?;
    let transcript = store.transcript(&[sourced, window]).await?;
    assert_eq!(
        transcript
            .iter()
            .map(|message| message.id)
            .collect::<Vec<_>>(),
        [late, early, start, tool]
    );
    let tool = transcript.last().ok_or("tool row missing")?;
    assert_eq!(tool.tool_name.as_deref(), Some("Bash"));
    let expected = serde_json::to_string(&Tool {
        command: "hostname",
    })?;
    let tool_args = tool.tool_args.as_ref().ok_or("tool arguments missing")?;
    assert_eq!(serde_json::to_string(tool_args)?, expected);
    assert_eq!(tool.tool_result.as_deref(), Some("tinbox\n"));
    assert_eq!(tool.at, at("2026-10-10T13:05:00Z")?);
    Ok(())
}
