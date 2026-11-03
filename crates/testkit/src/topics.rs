use std::fmt::Debug;
use std::str::FromStr;

use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;
use kernel::topics::Alias;
use kernel::topics::Change;
use kernel::topics::DeliveredVia;
use kernel::topics::HistoryMark;
use kernel::topics::NewTopic;
use kernel::topics::Placement;
use kernel::topics::PlacementOutcome;
use kernel::topics::PlacementSource;
use kernel::topics::Root;
use kernel::topics::Slug;
use kernel::topics::StartupContext;
use kernel::topics::StartupKind;
use kernel::topics::StartupSection;
use kernel::topics::StartupSource;
use kernel::topics::StartupStore;
use kernel::topics::Suggestion;
use kernel::topics::Topic;
use kernel::topics::TopicChanges;
use kernel::topics::TopicError;
use kernel::topics::TopicId;
use kernel::topics::TopicKey;
use kernel::topics::TopicKind;
use kernel::topics::TopicListing;
use kernel::topics::TopicName;
use kernel::topics::TopicQuery;
use kernel::topics::TopicStore;
use kernel::wiki::page::WikiSlug;
use wire::session_id::SessionId;

pub type Outcome = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, thiserror::Error)]
#[error("{what}: expected {expected}, got {actual}")]
pub struct Mismatch {
    what: &'static str,
    expected: String,
    actual: String,
}

pub(crate) fn same<T: PartialEq + Debug>(
    what: &'static str,
    actual: T,
    expected: T,
) -> Result<(), Mismatch> {
    match (actual, expected) {
        (actual, expected) if actual == expected => Ok(()),
        (actual, expected) => Err(Mismatch {
            what,
            expected: format!("{expected:?}"),
            actual: format!("{actual:?}"),
        }),
    }
}

#[must_use]
pub fn at(minute: u32) -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH + TimeDelta::seconds(1_790_000_000 + i64::from(minute) * 60)
}

pub fn session(native: &str) -> Result<SessionId, wire::session_id::SessionIdError> {
    SessionId::from_str(&format!("claude-code:{native}"))
}

pub fn new_topic(
    name: &str,
    aliases: &[&str],
    parent: Option<TopicId>,
) -> Result<Topic, TopicError> {
    Topic::create(
        TopicId::random(),
        NewTopic {
            kind: TopicKind::Work,
            name: TopicName::new(name)?,
            aliases: aliases
                .iter()
                .map(|alias| Alias::new(alias))
                .collect::<Result<_, _>>()?,
            parent,
            directory: None,
            owner: Some(crate::owner().map_err(|error| TopicError::Unavailable(Box::new(error)))?),
        },
        at(0),
    )
}

pub async fn work_topics(store: &dyn TopicStore) -> Result<Vec<TopicListing>, TopicError> {
    Ok(store
        .list()
        .await?
        .into_iter()
        .filter(|listing| listing.topic.kind == TopicKind::Work)
        .collect())
}

fn alias_texts(topic: &Topic) -> Vec<&str> {
    topic.aliases.iter().map(Alias::as_str).collect()
}

pub async fn topics_are_created_found_and_renamed(store: &dyn TopicStore) -> Outcome {
    let mut topic = new_topic("RivetHub Desktop", &["Tauri", "desktop app"], None)?;
    topic.directory = Some(Root::Local {
        node: "node-a".into(),
        path: "/home/user/acmekit".into(),
    });
    topic.created_at = at(0) + TimeDelta::nanoseconds(123_456_789);
    topic.updated_at = topic.created_at;
    let created = store.create(topic.clone()).await?;
    same("created", created.name.as_str(), topic.name.as_str())?;
    same(
        "created reads back unchanged",
        store.find(&TopicKey::Id(topic.id)).await?.as_ref(),
        Some(&created),
    )?;
    let topic = created;
    same(
        "by id",
        store.find(&TopicKey::Id(topic.id)).await?.as_ref(),
        Some(&topic),
    )?;
    same(
        "by slug",
        store
            .find(&TopicKey::parse("rivethub desktop")?)
            .await?
            .as_ref(),
        Some(&topic),
    )?;
    let renamed = store
        .update(
            topic.id,
            TopicChanges {
                name: Some(TopicName::new("Desktop Client")?),
                aliases: Some(vec![Alias::new("electron")?]),
                directory: Change::Set(Root::Shared {
                    path: "/srv/shared/acmekit".into(),
                }),
                ..TopicChanges::default()
            },
            at(3),
        )
        .await?;
    same("renamed slug", renamed.slug.as_str(), "desktop-client")?;
    same("replaced aliases", alias_texts(&renamed), vec!["electron"])?;
    same(
        "stored rename",
        store.find(&TopicKey::Id(topic.id)).await?.as_ref(),
        Some(&renamed),
    )?;
    same(
        "old slug still finds it",
        store
            .find(&TopicKey::parse("rivethub-desktop")?)
            .await?
            .map(|found| found.id),
        Some(topic.id),
    )?;
    let missing = store.find(&TopicKey::parse("no-such-topic")?).await?;
    same("missing", missing, None)?;
    let listed = work_topics(store).await?;
    same("listed", listed.len(), 1)?;
    Ok(())
}

pub async fn slugs_and_aliases_are_unique_across_topics(store: &dyn TopicStore) -> Outcome {
    let first = store.create(new_topic("Deploy", &["Ship"], None)?).await?;
    let clash = store.create(new_topic("deploy!", &[], None)?).await;
    if !matches!(clash, Err(TopicError::SlugTaken { .. })) {
        return Err(format!("a second deploy slug must be refused: {clash:?}").into());
    }
    let alias = store.create(new_topic("Release", &["SHIP"], None)?).await;
    if !matches!(alias, Err(TopicError::AliasTaken { ref alias }) if alias == "Ship") {
        return Err(format!("a case-folded alias clash must be refused: {alias:?}").into());
    }
    let other = store
        .create(new_topic("Release", &["train"], None)?)
        .await?;
    let renamed = store
        .update(
            other.id,
            TopicChanges {
                name: Some(TopicName::new("Deploy")?),
                ..TopicChanges::default()
            },
            at(1),
        )
        .await;
    if !matches!(renamed, Err(TopicError::SlugTaken { .. })) {
        return Err(format!("a rename onto a taken slug must be refused: {renamed:?}").into());
    }
    same("first kept", work_topics(store).await?.len(), 2)?;
    same(
        "first still owns its alias",
        store
            .find(&TopicKey::Id(first.id))
            .await?
            .map(|topic| alias_texts(&topic).join(",")),
        Some("Ship".to_owned()),
    )?;
    Ok(())
}

pub async fn topics_nest_up_to_four_levels(store: &dyn TopicStore) -> Outcome {
    let runtime = store.create(new_topic("Runtime", &[], None)?).await?;
    let topics = store
        .create(new_topic("Topics", &[], Some(runtime.id))?)
        .await?;
    let merge = store
        .create(new_topic("Merge", &[], Some(topics.id))?)
        .await?;
    let plan = store
        .create(new_topic("Plan", &[], Some(merge.id))?)
        .await?;
    same("fourth level kept", plan.parent, Some(merge.id))?;
    let fifth = store.create(new_topic("Steps", &[], Some(plan.id))?).await;
    if !matches!(fifth, Err(TopicError::TooDeep { max: 4 })) {
        return Err(format!("a fifth level must be refused: {fifth:?}").into());
    }
    let orphan = store
        .create(new_topic("Orphan", &[], Some(TopicId::random()))?)
        .await;
    if !matches!(orphan, Err(TopicError::NotFound { .. })) {
        return Err(format!("an unknown parent must be refused: {orphan:?}").into());
    }
    let looped = store
        .update(
            runtime.id,
            TopicChanges {
                parent: Change::Set(merge.id),
                ..TopicChanges::default()
            },
            at(1),
        )
        .await;
    if !matches!(looped, Err(TopicError::IntoOwnSubtree)) {
        return Err(format!("a topic cannot move under its own subtopic: {looped:?}").into());
    }
    let cleared = store
        .update(
            topics.id,
            TopicChanges {
                parent: Change::Clear,
                ..TopicChanges::default()
            },
            at(2),
        )
        .await?;
    same("cleared parent", cleared.parent, None)?;
    Ok(())
}

pub async fn placements_keep_your_choice(store: &dyn TopicStore) -> Outcome {
    let mine = store.create(new_topic("Mine", &[], None)?).await?;
    let suggested = store.create(new_topic("Suggested", &[], None)?).await?;
    let session = session("00000000-0000-4000-8000-000000000001")?;
    let owner = crate::owner()?;
    let placed = |topic: TopicId, by: PlacementSource, minute: u32| Placement {
        session: session.clone(),
        topic,
        by,
        at: at(minute),
        owner: Some(owner.clone()),
    };
    same("forming", store.placement(&session).await?, None)?;
    let first = placed(suggested.id, PlacementSource::Categorizer, 1);
    same(
        "categorizer places a forming session",
        store.place(first.clone()).await?,
        PlacementOutcome::Placed(first),
    )?;
    let yours = placed(mine.id, PlacementSource::You, 2);
    same(
        "you move it",
        store.place(yours.clone()).await?,
        PlacementOutcome::Placed(yours.clone()),
    )?;
    same(
        "categorizer cannot override you",
        store
            .place(placed(suggested.id, PlacementSource::Categorizer, 3))
            .await?,
        PlacementOutcome::Kept(yours.clone()),
    )?;
    same(
        "stored placement",
        store.placement(&session).await?,
        Some(yours.clone()),
    )?;
    let sessions = store.sessions(mine.id).await?;
    same(
        "sessions in topic",
        sessions
            .iter()
            .map(|placed| &placed.placement)
            .collect::<Vec<_>>(),
        vec![&yours],
    )?;
    let unknown = store
        .place(placed(TopicId::random(), PlacementSource::You, 4))
        .await;
    if !matches!(unknown, Err(TopicError::NotFound { .. })) {
        return Err(format!("an unknown topic cannot hold sessions: {unknown:?}").into());
    }
    same("unplaced", store.unplace(&session).await?, Some(yours))?;
    same("forming again", store.placement(&session).await?, None)?;
    same("unplace twice", store.unplace(&session).await?, None)?;
    Ok(())
}

pub async fn topics_close_and_sessions_are_marked_as_history(store: &dyn TopicStore) -> Outcome {
    let topic = store.create(new_topic("Release", &[], None)?).await?;
    same("open when created", topic.closed_at, None)?;
    let closed = store.set_closed(topic.id, Some(at(5)), at(5)).await?;
    same("closed", closed.closed_at, Some(at(5)))?;
    same(
        "listed as closed",
        store
            .list()
            .await?
            .into_iter()
            .find(|listing| listing.topic.id == topic.id)
            .and_then(|listing| listing.topic.closed_at),
        Some(at(5)),
    )?;
    same(
        "closing again moves the time",
        store
            .set_closed(topic.id, Some(at(7)), at(7))
            .await?
            .closed_at,
        Some(at(7)),
    )?;
    same(
        "reopened",
        store.set_closed(topic.id, None, at(8)).await?.closed_at,
        None,
    )?;
    let unknown = store
        .set_closed(TopicId::random(), Some(at(9)), at(9))
        .await;
    if !matches!(unknown, Err(TopicError::NotFound { .. })) {
        return Err(format!("an unknown topic cannot be closed: {unknown:?}").into());
    }
    let loose = session("00000000-0000-4000-8000-000000000031")?;
    let filed = session("00000000-0000-4000-8000-000000000032")?;
    let mark = |session: &SessionId, minute: u32| HistoryMark {
        session: session.clone(),
        at: at(minute),
    };
    same(
        "marked",
        store.mark_history(mark(&loose, 10)).await?,
        mark(&loose, 10),
    )?;
    store.mark_history(mark(&filed, 11)).await?;
    store
        .place(Placement {
            session: filed.clone(),
            topic: topic.id,
            by: PlacementSource::You,
            at: at(12),
            owner: Some(crate::owner()?),
        })
        .await?;
    store.unplace(&filed).await?;
    same(
        "newest mark first, kept through place and unplace",
        store.history().await?,
        vec![mark(&filed, 11), mark(&loose, 10)],
    )?;
    same(
        "marking again moves the time",
        store.mark_history(mark(&loose, 13)).await?,
        mark(&loose, 13),
    )?;
    store.unmark_history(&loose).await?;
    store.unmark_history(&loose).await?;
    same("unmarked", store.history().await?, vec![mark(&filed, 11)])?;
    Ok(())
}

pub async fn merges_move_placements_aliases_and_slugs(store: &dyn TopicStore) -> Outcome {
    let source = store
        .create(new_topic("Desktop App", &["electron", "Tauri"], None)?)
        .await?;
    let into = store
        .create(new_topic("RivetHub Desktop", &["rh-desktop"], None)?)
        .await?;
    let child = store
        .create(new_topic("Desktop Menus", &[], Some(source.id))?)
        .await?;
    let session = session("00000000-0000-4000-8000-000000000002")?;
    let placement = Placement {
        session: session.clone(),
        topic: source.id,
        by: PlacementSource::Categorizer,
        at: at(1),
        owner: Some(crate::owner()?),
    };
    store.place(placement.clone()).await?;
    let merged = store.merge(source.id, into.id, at(5)).await?;
    same(
        "merged aliases",
        alias_texts(&merged),
        vec!["Desktop App", "electron", "rh-desktop", "Tauri"],
    )?;
    same("merged updated", merged.updated_at, at(5))?;
    same(
        "source gone",
        store.find(&TopicKey::Id(source.id)).await?,
        None,
    )?;
    same(
        "old slug resolves",
        store
            .find(&TopicKey::parse("desktop-app")?)
            .await?
            .map(|topic| topic.id),
        Some(into.id),
    )?;
    same(
        "placement moved",
        store.placement(&session).await?.map(|moved| moved.topic),
        Some(into.id),
    )?;
    same(
        "sub-topic adopted",
        store
            .find(&TopicKey::Id(child.id))
            .await?
            .and_then(|topic| topic.parent),
        Some(into.id),
    )?;
    same(
        "redirect listed",
        store
            .redirects()
            .await?
            .into_iter()
            .map(|redirect| redirect.slug)
            .collect::<Vec<_>>(),
        vec![Slug::fold("desktop-app")?],
    )?;
    let hits = store.search(&TopicQuery::new("desktop a"), 10).await?;
    same(
        "search follows redirects",
        hits.iter().map(|hit| hit.topic.id).collect::<Vec<_>>(),
        vec![into.id],
    )?;
    Ok(())
}

pub async fn redirects_follow_a_second_merge(store: &dyn TopicStore) -> Outcome {
    let first = store.create(new_topic("Desktop App", &[], None)?).await?;
    let second = store
        .create(new_topic("RivetHub Desktop", &[], None)?)
        .await?;
    let last = store.create(new_topic("Clients", &[], None)?).await?;
    store.merge(first.id, second.id, at(5)).await?;
    store.merge(second.id, last.id, at(6)).await?;
    for slug in ["desktop-app", "rivethub-desktop", "clients"] {
        same(
            "redirects follow a second merge",
            store
                .find(&TopicKey::parse(slug)?)
                .await?
                .map(|topic| topic.id),
            Some(last.id),
        )?;
    }
    let refused = store.merge(last.id, last.id, at(7)).await;
    if !matches!(refused, Err(TopicError::SelfMerge)) {
        return Err(format!("a self merge must be refused: {refused:?}").into());
    }
    Ok(())
}

pub async fn suggestions_hold_until_the_session_is_placed(store: &dyn TopicStore) -> Outcome {
    let gateway = store.create(new_topic("Gateway", &[], None)?).await?;
    let mesh = store.create(new_topic("Mesh", &[], None)?).await?;
    let session = session("00000000-0000-4000-8000-000000000021")?;
    same("no suggestion yet", store.suggestion(&session).await?, None)?;
    let unsure = Suggestion {
        session: session.clone(),
        name: TopicName::new("Mesh retries")?,
        topic: Some(mesh.id),
        parent: Some(mesh.id),
    };
    store.suggest(unsure.clone()).await?;
    same(
        "stored suggestion",
        store.suggestion(&session).await?,
        Some(unsure.clone()),
    )?;
    store.merge(mesh.id, gateway.id, at(2)).await?;
    same(
        "a merge moves the unsure topic and the parent",
        store
            .suggestion(&session)
            .await?
            .map(|suggestion| (suggestion.topic, suggestion.parent)),
        Some((Some(gateway.id), Some(gateway.id))),
    )?;
    let renamed = Suggestion {
        name: TopicName::new("Retry budget")?,
        topic: None,
        parent: None,
        ..unsure
    };
    store.suggest(renamed.clone()).await?;
    same(
        "a new suggestion replaces the old one",
        store.suggestion(&session).await?,
        Some(renamed.clone()),
    )?;
    store
        .place(Placement {
            session: session.clone(),
            topic: gateway.id,
            by: PlacementSource::Categorizer,
            at: at(3),
            owner: Some(crate::owner()?),
        })
        .await?;
    same(
        "a placement clears the suggestion",
        store.suggestion(&session).await?,
        None,
    )?;
    store.suggest(renamed).await?;
    same(
        "a placed session takes no suggestion",
        store.suggestion(&session).await?,
        None,
    )?;
    Ok(())
}

pub async fn startup_context_is_kept_per_session(store: &dyn StartupStore) -> Outcome {
    let first = session("startup-one")?;
    same("before any start", store.startup(&first).await?, None)?;
    let empty = StartupContext {
        topic: TopicId::random(),
        sections: Vec::new(),
        tokens: 0,
        delivered_via: DeliveredVia::None,
    };
    store.keep_startup(&first, &empty, at(1)).await?;
    same(
        "an empty context",
        store.startup(&first).await?,
        Some(empty),
    )?;
    let kept = StartupContext {
        topic: TopicId::random(),
        sections: vec![StartupSection {
            kind: StartupKind::Topic,
            title: "Mesh".to_owned(),
            text: "The mesh runs step-ca.".to_owned(),
            sources: vec![
                StartupSource::Wiki(WikiSlug::try_from("mesh".to_owned())?),
                StartupSource::Session(session("startup-two")?),
            ],
        }],
        tokens: 7,
        delivered_via: DeliveredVia::MetaAppend,
    };
    store.keep_startup(&first, &kept, at(2)).await?;
    same(
        "the latest context",
        store.startup(&first).await?,
        Some(kept),
    )?;
    same(
        "another session",
        store.startup(&session("startup-two")?).await?,
        None,
    )?;
    Ok(())
}
