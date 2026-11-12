use std::collections::BTreeMap;

use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use kernel::memory::MemoryEntry;
use kernel::memory::MemoryStore;
use kernel::memory::SessionActivity;
use kernel::memory::SessionDeletion;
use kernel::topics::Placement;
use kernel::topics::PlacementSource;
use kernel::topics::TopicStore;
use wire::message::Role;
use wire::session_id::SessionId;

use crate::topics::Outcome;
use crate::topics::at;
use crate::topics::new_topic;
use crate::topics::same;
use crate::topics::session;

fn keys(sessions: &[&SessionId]) -> Vec<String> {
    sessions
        .iter()
        .map(|session| session.as_str().to_owned())
        .collect()
}

pub const DELETED: &str = "00000000-0000-4000-8000-0000000000d1";
pub const SIBLING: &str = "00000000-0000-4000-8000-0000000000d2";

#[must_use]
pub fn message(event_id: &str, role: Role, content: &str) -> CaptureMessage {
    CaptureMessage {
        event_id: event_id.to_owned(),
        role,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: Some(at(1)),
    }
}

#[must_use]
pub fn batch(
    session: &SessionId,
    agent: &str,
    messages: Vec<CaptureMessage>,
) -> CaptureBatchRequest {
    CaptureBatchRequest {
        session_key: session.as_str().to_owned(),
        agent: agent.to_owned(),
        channel: Some("cli".to_owned()),
        title: Some("A session".to_owned()),
        settings: None,
        task_id: None,
        finalize: None,
        created_at: Some(at(1)),
        updated_at: Some(at(2)),
        messages,
    }
}

#[must_use]
pub fn exchange(prefix: &str) -> Vec<CaptureMessage> {
    vec![
        message(
            &format!("{prefix}-1"),
            Role::User,
            "deploy the release branch",
        ),
        message(
            &format!("{prefix}-2"),
            Role::Assistant,
            "Deployed build 42.",
        ),
    ]
}

pub async fn seed(
    memory: &dyn MemoryStore,
) -> Result<(SessionId, SessionId), Box<dyn std::error::Error>> {
    let deleted = session(DELETED)?;
    let sibling = session(SIBLING)?;
    memory
        .capture(
            batch(&deleted, "opus", exchange("a")),
            &crate::owner()?,
            None,
        )
        .await?;
    memory
        .capture(
            batch(&deleted, "sonnet", exchange("b")),
            &crate::owner()?,
            None,
        )
        .await?;
    memory
        .capture(
            batch(&sibling, "opus", exchange("c")),
            &crate::owner()?,
            None,
        )
        .await?;
    Ok((deleted, sibling))
}

#[expect(
    clippy::too_many_lines,
    reason = "one scenario whose delete, resend and append steps read in order"
)]
pub async fn capture_for_a_deleted_session_writes_nothing(
    memory: &dyn MemoryStore,
    deletions: &dyn SessionDeletion,
) -> Outcome {
    let (deleted, sibling) = seed(memory).await?;
    let erased = deletions
        .delete_session(deleted.as_str(), Some("user-1"), at(5))
        .await?;
    same("conversations across agents", erased.conversations, 2)?;
    same("messages across agents", erased.messages, 4)?;
    same(
        "tombstoned",
        deletions.tombstones(&keys(&[&deleted, &sibling])).await?,
        BTreeMap::from([(deleted.as_str().to_owned(), at(5))]),
    )?;
    let resent = memory
        .capture(
            batch(&deleted, "opus", exchange("again")),
            &crate::owner()?,
            None,
        )
        .await?;
    same("capture inserts nothing", resent.inserted, 0)?;
    same("capture reports the batch skipped", resent.skipped, 2)?;
    let mut unstamped = exchange("unstamped");
    for message in &mut unstamped {
        message.created_at = None;
    }
    let unstamped = memory
        .capture(batch(&deleted, "opus", unstamped), &crate::owner()?, None)
        .await?;
    same(
        "messages without a timestamp count as old",
        unstamped.inserted,
        0,
    )?;
    let appended = memory
        .append(
            &crate::owner()?,
            vec![MemoryEntry {
                provenance: None,
                id: Some("append-1".to_owned()),
                session_id: deleted.as_str().to_owned(),
                agent: "opus".to_owned(),
                channel: "cli".to_owned(),
                role: Role::User,
                content: "one more".to_owned(),
                tool_name: None,
                tool_args: None,
                tool_result: None,
                metadata: None,
                created_at: Some(at(5)),
            }],
        )
        .await?;
    same("append inserts nothing", appended, 0)?;
    same(
        "the tombstone holds",
        deletions.tombstones(&keys(&[&deleted])).await?.len(),
        1,
    )?;
    let activity = deletions
        .session_activity(&keys(&[&deleted, &sibling]))
        .await?;
    same(
        "deleted session has no rows",
        activity.get(deleted.as_str()).cloned(),
        Some(SessionActivity::default()),
    )?;
    same(
        "sibling keeps its rows",
        activity.get(sibling.as_str()).map(|entry| entry.messages),
        Some(2),
    )?;
    let sibling_more = memory
        .capture(
            batch(&sibling, "opus", exchange("d")),
            &crate::owner()?,
            None,
        )
        .await?;
    same("sibling still captures", sibling_more.inserted, 2)?;
    Ok(())
}

pub async fn activity_counts_turns_tools_and_placements(
    memory: &dyn MemoryStore,
    deletions: &dyn SessionDeletion,
    topics: &dyn TopicStore,
) -> Outcome {
    let quiet = session("00000000-0000-4000-8000-0000000000a1")?;
    let busy = session("00000000-0000-4000-8000-0000000000a2")?;
    let placed = session("00000000-0000-4000-8000-0000000000a3")?;
    let absent = session("00000000-0000-4000-8000-0000000000a4")?;
    let owner = crate::owner()?;
    memory
        .capture(
            batch(
                &quiet,
                "opus",
                vec![
                    message("q-1", Role::User, "hi"),
                    message("q-0", Role::User, "  \n "),
                    message("q-2", Role::Assistant, "Hello!"),
                ],
            ),
            &owner,
            None,
        )
        .await?;
    let mut tool = message("b-2", Role::Tool, "");
    tool.tool_name = Some("Bash".to_owned());
    tool.tool_result = Some("ok".to_owned());
    memory
        .capture(
            batch(
                &busy,
                "opus",
                vec![message("b-1", Role::User, "list files"), tool],
            ),
            &owner,
            None,
        )
        .await?;
    memory
        .capture(batch(&placed, "opus", exchange("p")), &owner, None)
        .await?;
    let topic = topics.create(new_topic("Kept", &[], None)?).await?;
    topics
        .place(Placement {
            session: placed.clone(),
            topic: topic.id,
            by: PlacementSource::You,
            at: at(3),
            owner: Some(owner),
        })
        .await?;
    let activity = deletions
        .session_activity(&keys(&[&quiet, &busy, &placed, &absent]))
        .await?;
    same(
        "quiet",
        activity.get(quiet.as_str()).cloned(),
        Some(SessionActivity {
            conversations: 1,
            messages: 3,
            user_messages: 1,
            assistant_messages: 1,
            reply: Some("Hello!".to_owned()),
            ..SessionActivity::default()
        }),
    )?;
    same(
        "busy has a tool row",
        activity.get(busy.as_str()).map(|entry| entry.tool_rows),
        Some(1),
    )?;
    same(
        "placed",
        activity.get(placed.as_str()).map(|entry| entry.placed),
        Some(true),
    )?;
    same(
        "absent",
        activity.get(absent.as_str()).cloned(),
        Some(SessionActivity::default()),
    )?;
    Ok(())
}

pub async fn a_deleted_session_used_again_comes_back(
    memory: &dyn MemoryStore,
    deletions: &dyn SessionDeletion,
) -> Outcome {
    let (deleted, _sibling) = seed(memory).await?;
    deletions
        .delete_session(deleted.as_str(), None, at(5))
        .await?;
    let mut resumed = exchange("resumed");
    if let Some(reply) = resumed.get_mut(1) {
        reply.created_at = Some(at(7));
    }
    let revived = memory
        .capture(batch(&deleted, "opus", resumed), &crate::owner()?, None)
        .await?;
    same("the whole batch is captured", revived.inserted, 2)?;
    same(
        "the tombstone is lifted",
        deletions.tombstones(&keys(&[&deleted])).await?,
        BTreeMap::new(),
    )?;
    let activity = deletions.session_activity(&keys(&[&deleted])).await?;
    same(
        "memory holds the resumed work",
        activity.get(deleted.as_str()).map(|entry| entry.messages),
        Some(2),
    )?;
    deletions
        .delete_session(deleted.as_str(), None, at(10))
        .await?;
    let appended = memory
        .append(
            &crate::owner()?,
            vec![
                MemoryEntry {
                    provenance: None,
                    id: Some("append-old".to_owned()),
                    session_id: deleted.as_str().to_owned(),
                    agent: "opus".to_owned(),
                    channel: "cli".to_owned(),
                    role: Role::User,
                    content: "an old line".to_owned(),
                    tool_name: None,
                    tool_args: None,
                    tool_result: None,
                    metadata: None,
                    created_at: Some(at(9)),
                },
                MemoryEntry {
                    provenance: None,
                    id: Some("append-new".to_owned()),
                    session_id: deleted.as_str().to_owned(),
                    agent: "opus".to_owned(),
                    channel: "cli".to_owned(),
                    role: Role::Assistant,
                    content: "a new reply".to_owned(),
                    tool_name: None,
                    tool_args: None,
                    tool_result: None,
                    metadata: None,
                    created_at: Some(at(11)),
                },
            ],
        )
        .await?;
    same("append keeps the whole batch", appended, 2)?;
    same(
        "append lifts the tombstone",
        deletions.tombstones(&keys(&[&deleted])).await?,
        BTreeMap::new(),
    )?;
    Ok(())
}

pub async fn an_empty_session_is_reported_until_it_is_deleted(
    memory: &dyn MemoryStore,
    deletions: &dyn SessionDeletion,
) -> Outcome {
    seed(memory).await?;
    let empty = session("00000000-0000-4000-8000-0000000000e1")?;
    memory
        .capture(batch(&empty, "grok", Vec::new()), &crate::owner()?, None)
        .await?;
    same(
        "only the session without messages is empty",
        deletions.empty_sessions(10).await?,
        vec![empty.as_str().to_owned()],
    )?;
    deletions
        .delete_session(empty.as_str(), None, at(5))
        .await?;
    same(
        "a deleted session is no longer reported",
        deletions.empty_sessions(10).await?,
        Vec::<String>::new(),
    )?;
    Ok(())
}

pub async fn a_kept_session_is_remembered_from_its_latest_keep(
    deletions: &dyn SessionDeletion,
) -> Outcome {
    let kept = session(DELETED)?;
    let other = session(SIBLING)?;
    let asked = keys(&[&kept, &other]);
    same(
        "nothing is kept before a keep",
        deletions.kept(&asked).await?,
        BTreeMap::new(),
    )?;
    deletions
        .keep_session(kept.as_str(), Some("owner"), at(3))
        .await?;
    deletions
        .keep_session(kept.as_str(), Some("owner"), at(7))
        .await?;
    same(
        "only the kept session is named, at its latest keep",
        deletions.kept(&asked).await?,
        BTreeMap::from([(kept.as_str().to_owned(), at(7))]),
    )?;
    Ok(())
}
