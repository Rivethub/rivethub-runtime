use std::collections::BTreeSet;

use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use kernel::memory::Limit;
use kernel::memory::MemoryScope;
use kernel::memory::MemoryState;
use kernel::memory::MemoryStore;
use kernel::memory::SearchMode;
use kernel::memory::SearchQuery;
use kernel::memory::SessionCatalog;
use kernel::memory::SessionDeletion;
use kernel::memory::SessionQuery;
use kernel::memory::SessionView;
use kernel::topics::Placement;
use kernel::topics::PlacementSource;
use kernel::topics::TopicStore;
use wire::harness::HarnessId;
use wire::message::Role;
use wire::session_id::SessionId;

use crate::topics::Outcome;
use crate::topics::at;
use crate::topics::new_topic;
use crate::topics::same;

pub const ALPHA: &str = "00000000-0000-4000-8000-0000000000c1";
pub const BETA: &str = "00000000-0000-4000-8000-0000000000c2";
pub const GAMMA: &str = "00000000-0000-4000-8000-0000000000c3";
pub const DELTA: &str = "00000000-0000-4000-8000-0000000000c4";

#[derive(Debug)]
pub struct Seeded {
    pub alpha: SessionId,
    pub beta: SessionId,
    pub gamma: SessionId,
    pub delta: SessionId,
}

fn turn(event: &str, role: Role, content: &str, minute: u32) -> CaptureMessage {
    CaptureMessage {
        event_id: event.to_owned(),
        role,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: Some(at(minute)),
    }
}

struct Spec<'a> {
    session: &'a SessionId,
    title: &'a str,
    minute: u32,
    pairs: u32,
    finalize: bool,
    cwd: Option<&'a str>,
}

async fn capture(memory: &dyn MemoryStore, spec: Spec<'_>) -> Outcome {
    let Spec {
        session,
        title,
        minute,
        pairs,
        finalize,
        cwd,
    } = spec;
    let word = title.to_lowercase();
    let mut messages = Vec::new();
    for pair in 0..pairs {
        let stamp = minute + pair;
        messages.push(turn(
            &format!("{word}-u{pair}"),
            Role::User,
            &format!("quokka {word} question {pair}"),
            stamp,
        ));
        messages.push(turn(
            &format!("{word}-a{pair}"),
            Role::Assistant,
            &format!("answer {pair} about {word}"),
            stamp,
        ));
    }
    let settings = cwd
        .map(|cwd| wire::JsonObject::from_serialize(&serde_json::json!({ "cwd": cwd })))
        .transpose()?;
    memory
        .capture(
            CaptureBatchRequest {
                session_key: session.as_str().to_owned(),
                agent: "opus".to_owned(),
                channel: Some("cli".to_owned()),
                title: Some(title.to_owned()),
                settings,
                task_id: None,
                finalize: finalize.then_some(true),
                created_at: Some(at(minute)),
                updated_at: Some(at(minute + pairs)),
                messages,
            },
            &crate::owner()?,
            None,
        )
        .await?;
    Ok(())
}

pub async fn seed(memory: &dyn MemoryStore) -> Result<Seeded, Box<dyn std::error::Error>> {
    let alpha = SessionId::new(HarnessId::ClaudeCode, ALPHA);
    let beta = SessionId::new(HarnessId::ClaudeCode, BETA);
    let gamma = SessionId::new(HarnessId::Codex, GAMMA);
    let delta = SessionId::new(HarnessId::ClaudeCode, DELTA);
    for (session, title, minute, pairs, finalize, cwd) in [
        (&alpha, "Alpha", 10, 1, true, Some("/home/user/alpha")),
        (&beta, "Beta", 20, 2, false, None),
        (&gamma, "Gamma", 30, 1, true, None),
        (&delta, "Delta", 40, 1, true, None),
    ] {
        capture(
            memory,
            Spec {
                session,
                title,
                minute,
                pairs,
                finalize,
                cwd,
            },
        )
        .await?;
    }
    Ok(Seeded {
        alpha,
        beta,
        gamma,
        delta,
    })
}

fn ids(items: &[kernel::memory::SessionRecord]) -> Vec<String> {
    items.iter().map(|item| item.id.to_string()).collect()
}

fn keys(sessions: &[&SessionId]) -> Vec<String> {
    sessions.iter().map(ToString::to_string).collect()
}

async fn listed(
    catalog: &dyn SessionCatalog,
    query: SessionQuery,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(ids(&catalog.list(&query).await?.items))
}

async fn quokka_sessions(
    memory: &dyn MemoryStore,
) -> Result<BTreeSet<String>, kernel::memory::MemoryStoreError> {
    let outcome = memory
        .search(SearchQuery {
            text: "quokka".to_owned(),
            mode: SearchMode::Fts,
            scope: MemoryScope::Messages,
            agent: None,
            role: None,
            exclude_roles: Vec::new(),
            tool_name: None,
            tag: None,
            limit: Limit::clamped(Some(50), 50, 1..=50),
            since: None,
            before: None,
            window: None,
            expand: None,
            synthesize: None,
        })
        .await?;
    Ok(outcome
        .results
        .into_iter()
        .filter_map(|hit| hit.session_id)
        .collect())
}

pub async fn lists_sessions_newest_first_with_counts_and_filters(
    memory: &dyn MemoryStore,
    catalog: &dyn SessionCatalog,
) -> Outcome {
    let seeded = seed(memory).await?;
    let everything = catalog.list(&SessionQuery::new(SessionView::All)).await?;
    same(
        "newest first",
        ids(&everything.items),
        keys(&[&seeded.delta, &seeded.gamma, &seeded.beta, &seeded.alpha]),
    )?;
    same("no next page", everything.next.clone(), None)?;
    let counts: Vec<u64> = everything
        .items
        .iter()
        .map(|item| item.message_count)
        .collect();
    same("message counts", counts, vec![2, 2, 4, 2])?;
    let states: Vec<MemoryState> = everything.items.iter().map(|item| item.memory).collect();
    same(
        "capturing until finalized",
        states,
        vec![
            MemoryState::Queued,
            MemoryState::Queued,
            MemoryState::Capturing,
            MemoryState::Queued,
        ],
    )?;
    let alpha = everything
        .items
        .iter()
        .find(|item| item.id == seeded.alpha)
        .ok_or("alpha missing")?;
    same("title", alpha.title.clone(), "Alpha".to_owned())?;
    same("started", alpha.started_at, at(10))?;
    same("ended once finalized", alpha.ended_at.is_some(), true)?;
    same(
        "cwd",
        alpha.cwd.clone(),
        Some("/home/user/alpha".to_owned()),
    )?;
    same("not kept", alpha.kept, false)?;
    same("not trashed", alpha.trashed, false)?;
    let beta = everything
        .items
        .iter()
        .find(|item| item.id == seeded.beta)
        .ok_or("beta missing")?;
    same("a live session has not ended", beta.ended_at, None)?;

    pages_and_filters(catalog, &seeded).await
}

async fn pages_and_filters(catalog: &dyn SessionCatalog, seeded: &Seeded) -> Outcome {
    let mut first = SessionQuery::new(SessionView::All);
    first.limit = 3;
    let page = catalog.list(&first).await?;
    same(
        "first page",
        ids(&page.items),
        keys(&[&seeded.delta, &seeded.gamma, &seeded.beta]),
    )?;
    let mut second = SessionQuery::new(SessionView::All);
    second.limit = 3;
    second.after = page.next;
    let rest = catalog.list(&second).await?;
    same("second page", ids(&rest.items), keys(&[&seeded.alpha]))?;
    same("second page is the last", rest.next, None)?;

    let mut codex = SessionQuery::new(SessionView::All);
    codex.harness = Some(HarnessId::Codex);
    same(
        "harness filter",
        listed(catalog, codex).await?,
        keys(&[&seeded.gamma]),
    )?;
    let mut text = SessionQuery::new(SessionView::All);
    text.text = Some("ALPH".to_owned());
    same(
        "text filter is a case-insensitive title match",
        listed(catalog, text).await?,
        keys(&[&seeded.alpha]),
    )?;
    let mut wild = SessionQuery::new(SessionView::All);
    wild.text = Some("%".to_owned());
    same(
        "text filter takes wildcards literally",
        listed(catalog, wild).await?,
        Vec::new(),
    )?;
    let found = catalog.get(&seeded.beta).await?.ok_or("beta get")?;
    same("detail count", found.message_count, 4)?;
    let absent = SessionId::new(
        HarnessId::ClaudeCode,
        "00000000-0000-4000-8000-0000000000ff",
    );
    same("unknown session", catalog.get(&absent).await?, None)?;
    Ok(())
}

pub async fn keep_and_topics_shape_the_views(
    memory: &dyn MemoryStore,
    catalog: &dyn SessionCatalog,
    deletions: &dyn SessionDeletion,
    topics: &dyn TopicStore,
) -> Outcome {
    let seeded = seed(memory).await?;
    let parent = topics.create(new_topic("Platform", &[], None)?).await?;
    let child = topics
        .create(new_topic("Deploys", &[], Some(parent.id))?)
        .await?;
    topics
        .place(Placement {
            session: seeded.alpha.clone(),
            topic: child.id,
            by: PlacementSource::You,
            at: at(50),
            owner: Some(crate::owner()?),
        })
        .await?;
    deletions
        .keep_session(seeded.gamma.as_str(), Some("user-1"), at(51))
        .await?;

    same(
        "unfiled leaves the placed session out",
        listed(catalog, SessionQuery::new(SessionView::Unfiled)).await?,
        keys(&[&seeded.delta, &seeded.gamma, &seeded.beta]),
    )?;
    same(
        "kept view",
        listed(catalog, SessionQuery::new(SessionView::Kept)).await?,
        keys(&[&seeded.gamma]),
    )?;
    let mut by_topic = SessionQuery::new(SessionView::All);
    by_topic.topic = Some(child.id);
    same(
        "topic filter",
        listed(catalog, by_topic).await?,
        keys(&[&seeded.alpha]),
    )?;
    let mut by_parent = SessionQuery::new(SessionView::All);
    by_parent.topic = Some(parent.id);
    same(
        "a parent topic includes its sub-topics",
        listed(catalog, by_parent).await?,
        keys(&[&seeded.alpha]),
    )?;
    let placed = catalog.get(&seeded.alpha).await?.ok_or("alpha get")?;
    same(
        "topic path runs from the root",
        placed.topic.map(|topic| (topic.id, topic.path)),
        Some((child.id, vec!["Platform".to_owned(), "Deploys".to_owned()])),
    )?;
    let kept = catalog.get(&seeded.gamma).await?.ok_or("gamma get")?;
    same("kept flag", kept.kept, true)?;

    deletions.unkeep_session(seeded.gamma.as_str()).await?;
    same(
        "un-kept",
        listed(catalog, SessionQuery::new(SessionView::Kept)).await?,
        Vec::new(),
    )?;
    deletions.unkeep_session(seeded.gamma.as_str()).await?;
    Ok(())
}

pub async fn trash_is_reversible_and_leaves_recall(
    memory: &dyn MemoryStore,
    catalog: &dyn SessionCatalog,
    deletions: &dyn SessionDeletion,
) -> Outcome {
    let seeded = seed(memory).await?;
    let before = quokka_sessions(memory).await?;
    same("all four recalled", before.len(), 4)?;
    deletions
        .keep_session(seeded.delta.as_str(), None, at(60))
        .await?;

    deletions
        .trash_session(seeded.delta.as_str(), Some("user-1"), at(61))
        .await?;
    deletions
        .trash_session(seeded.delta.as_str(), Some("user-1"), at(62))
        .await?;
    same(
        "trashed leaves all",
        listed(catalog, SessionQuery::new(SessionView::All)).await?,
        keys(&[&seeded.gamma, &seeded.beta, &seeded.alpha]),
    )?;
    same(
        "trashed leaves unfiled",
        listed(catalog, SessionQuery::new(SessionView::Unfiled)).await?,
        keys(&[&seeded.gamma, &seeded.beta, &seeded.alpha]),
    )?;
    same(
        "trashed leaves kept",
        listed(catalog, SessionQuery::new(SessionView::Kept)).await?,
        Vec::new(),
    )?;
    let trash = catalog.list(&SessionQuery::new(SessionView::Trash)).await?;
    same("trash view", ids(&trash.items), keys(&[&seeded.delta]))?;
    same(
        "trashed flag",
        trash.items.first().map(|item| (item.trashed, item.kept)),
        Some((true, true)),
    )?;
    let marks = deletions
        .trashed(&keys(&[&seeded.delta, &seeded.alpha]))
        .await?;
    same(
        "trash mark keeps the first moment",
        marks.get(seeded.delta.as_str()).copied(),
        Some(at(61)),
    )?;
    let recalled = quokka_sessions(memory).await?;
    same(
        "search leaves the trashed session",
        recalled.contains(seeded.delta.as_str()),
        false,
    )?;
    same("search keeps the others", recalled.len(), 3)?;

    deletions.restore_session(seeded.delta.as_str()).await?;
    deletions.restore_session(seeded.delta.as_str()).await?;
    same(
        "restored returns",
        listed(catalog, SessionQuery::new(SessionView::All)).await?,
        keys(&[&seeded.delta, &seeded.gamma, &seeded.beta, &seeded.alpha]),
    )?;
    same(
        "restored is recalled",
        quokka_sessions(memory).await?.len(),
        4,
    )?;
    same(
        "the trash view is empty again",
        listed(catalog, SessionQuery::new(SessionView::Trash)).await?,
        Vec::new(),
    )?;
    Ok(())
}

pub async fn purge_removes_only_old_trash_through_deletion(
    memory: &dyn MemoryStore,
    catalog: &dyn SessionCatalog,
    deletions: &dyn SessionDeletion,
) -> Outcome {
    let seeded = seed(memory).await?;
    deletions
        .trash_session(seeded.alpha.as_str(), None, at(100))
        .await?;
    deletions
        .trash_session(seeded.beta.as_str(), None, at(200))
        .await?;
    let none = deletions.purge_trashed(at(50), 10).await?;
    same("nothing is old enough", none.conversations, 0)?;
    let purged = deletions.purge_trashed(at(150), 10).await?;
    same("one conversation purged", purged.conversations, 1)?;
    same("its messages purged", purged.messages, 2)?;
    same(
        "alpha is gone for good",
        catalog.get(&seeded.alpha).await?,
        None,
    )?;
    same(
        "alpha is tombstoned",
        deletions
            .tombstones(&keys(&[&seeded.alpha]))
            .await?
            .contains_key(seeded.alpha.as_str()),
        true,
    )?;
    same(
        "alpha's trash mark went with it",
        deletions
            .trashed(&keys(&[&seeded.alpha]))
            .await?
            .contains_key(seeded.alpha.as_str()),
        false,
    )?;
    same(
        "beta is still in the trash",
        listed(catalog, SessionQuery::new(SessionView::Trash)).await?,
        keys(&[&seeded.beta]),
    )?;
    Ok(())
}

pub async fn excludes_children_and_derives_titles(
    memory: &dyn MemoryStore,
    catalog: &dyn SessionCatalog,
) -> Outcome {
    let seeded = seed(memory).await?;
    let child = SessionId::new(
        HarnessId::ClaudeCode,
        "00000000-0000-4000-8000-0000000000c9",
    );
    let mut messages = vec![turn(
        "child-u0",
        Role::User,
        "Read /home/user/.claude/skills/poteto-mode/SKILL.md in full first",
        50,
    )];
    messages.push(turn("child-a0", Role::Assistant, "ok", 50));
    memory
        .capture(
            CaptureBatchRequest {
                session_key: child.as_str().to_owned(),
                agent: "opus".to_owned(),
                channel: Some("cli".to_owned()),
                title: Some(child.native().to_owned()),
                settings: None,
                task_id: None,
                finalize: Some(true),
                created_at: Some(at(50)),
                updated_at: Some(at(51)),
                messages,
            },
            &crate::owner()?,
            None,
        )
        .await?;
    let all = catalog.list(&SessionQuery::new(SessionView::All)).await?;
    let titled = all
        .items
        .iter()
        .find(|item| item.id == child)
        .ok_or("child missing")?;
    same(
        "an untitled session is named from its first message",
        titled.title.clone(),
        "Read SKILL.md in full first".to_owned(),
    )?;
    let mut hidden = SessionQuery::new(SessionView::All);
    hidden.exclude = vec![child.to_string()];
    same(
        "excluded keys leave the list",
        listed(catalog, hidden).await?,
        keys(&[&seeded.delta, &seeded.gamma, &seeded.beta, &seeded.alpha]),
    )?;
    same(
        "get ignores nothing a caller can exclude",
        catalog.get(&child).await?.map(|record| record.id),
        Some(child),
    )?;
    Ok(())
}

pub const BARE: &str = "00000000-0000-4000-8000-0000000000c5";
pub const CLI_UUID: &str = "00000000-0000-4000-8000-0000000000c6";

pub(crate) async fn capture_stored(
    memory: &dyn MemoryStore,
    key: &str,
    channel: &str,
    title: &str,
    minute: u32,
) -> Outcome {
    let word = title.to_lowercase().replace(' ', "-");
    memory
        .capture(
            CaptureBatchRequest {
                session_key: key.to_owned(),
                agent: format!("agent-{word}"),
                channel: Some(channel.to_owned()),
                title: Some(title.to_owned()),
                settings: None,
                task_id: None,
                finalize: Some(true),
                created_at: Some(at(minute)),
                updated_at: Some(at(minute + 1)),
                messages: vec![
                    turn(&format!("{word}-u"), Role::User, title, minute),
                    turn(&format!("{word}-a"), Role::Assistant, "ok", minute),
                ],
            },
            &crate::owner()?,
            None,
        )
        .await?;
    Ok(())
}

pub async fn lists_bare_claude_sessions_under_their_harness(
    memory: &dyn MemoryStore,
    catalog: &dyn SessionCatalog,
) -> Outcome {
    let seeded = seed(memory).await?;
    let bare = SessionId::new(HarnessId::ClaudeCode, BARE);
    capture_stored(memory, BARE, "claude-code", "Bare start", 60).await?;
    capture_stored(memory, bare.as_str(), "claude-code", "Bare follow-up", 70).await?;
    capture_stored(memory, CLI_UUID, "cli", "Not a session", 80).await?;
    let all = catalog.list(&SessionQuery::new(SessionView::All)).await?;
    same(
        "a bare Claude Code key lists once as its claude-code session; a bare key on another channel names none",
        ids(&all.items),
        keys(&[
            &bare,
            &seeded.delta,
            &seeded.gamma,
            &seeded.beta,
            &seeded.alpha,
        ]),
    )?;
    let newest = all.items.first().ok_or("bare session missing")?;
    same("both stored keys count", newest.message_count, 4)?;
    same("started with the bare key", newest.started_at, at(60))?;
    same(
        "titled by the newest conversation",
        newest.title.clone(),
        "Bare follow-up".to_owned(),
    )?;
    let mut claude = SessionQuery::new(SessionView::All);
    claude.harness = Some(HarnessId::ClaudeCode);
    same(
        "the harness filter keeps it",
        listed(catalog, claude).await?.first().cloned(),
        Some(bare.to_string()),
    )?;
    let mut text = SessionQuery::new(SessionView::All);
    text.text = Some("follow-up".to_owned());
    same(
        "text search finds it",
        listed(catalog, text).await?,
        keys(&[&bare]),
    )?;
    let found = catalog
        .get(&bare)
        .await?
        .ok_or("get misses the bare session")?;
    same("get counts both stored keys", found.message_count, 4)?;
    Ok(())
}
