use chrono::NaiveDate;
use chrono::TimeDelta;
use chrono::Utc;
use kernel::changes::Change;
use kernel::changes::ChangeActor;
use kernel::changes::ChangeFeed;
use kernel::changes::ChangeKind;
use kernel::changes::ChangeWindow;
use kernel::changes::DailyDigest;
use kernel::changes::DigestRequest;
use kernel::changes::DigestStore;
use kernel::changes::SubjectKey;
use kernel::facts::Evidence;
use kernel::facts::FactKey;
use kernel::facts::FactStore;
use kernel::facts::FactValue;
use kernel::facts::NewClaim;
use kernel::facts::Said;
use kernel::memory::MemoryStore;
use kernel::memory::TagAdd;
use kernel::memory::TagEntity;
use kernel::memory::TagEntityType;
use kernel::owner::Owner;
use kernel::topics::Decision;
use kernel::topics::DecisionStore;
use kernel::topics::DecisionText;
use kernel::topics::NewTopic;
use kernel::topics::Placement;
use kernel::topics::PlacementSource;
use kernel::topics::Topic;
use kernel::topics::TopicId;
use kernel::topics::TopicKind;
use kernel::topics::TopicName;
use kernel::topics::TopicStore;
use uuid::Uuid;
use wire::session_id::SessionId;

use crate::topics::Outcome;
use crate::topics::at;
use crate::topics::same;
use crate::topics::session;

const FILED: &str = "00000000-0000-4000-8000-0000000000c1";

async fn topic(
    store: &dyn TopicStore,
    name: &str,
    kind: TopicKind,
    parent: Option<TopicId>,
    minute: u32,
) -> Result<Topic, Box<dyn std::error::Error>> {
    let created = Topic::create(
        TopicId::random(),
        NewTopic {
            kind,
            name: TopicName::new(name)?,
            aliases: Vec::new(),
            parent,
            directory: None,
            owner: Some(crate::owner()?),
        },
        at(minute),
    )?;
    Ok(store.create(created).await?)
}

async fn hosts(store: &dyn TopicStore) -> Result<TopicId, Box<dyn std::error::Error>> {
    Ok(store
        .list()
        .await?
        .into_iter()
        .find(|listing| {
            listing.topic.kind == TopicKind::Type && listing.topic.name.as_str() == "Hosts"
        })
        .ok_or("the seeded Hosts type is missing")?
        .topic
        .id)
}

fn shown(changes: &[Change]) -> Vec<(ChangeKind, String, Option<String>)> {
    changes
        .iter()
        .map(|change| {
            (
                change.kind,
                change.text.clone(),
                change.source.as_ref().map(ToString::to_string),
            )
        })
        .collect()
}

async fn seed<S>(store: &S) -> Result<(SessionId, TopicId), Box<dyn std::error::Error>>
where
    S: TopicStore + FactStore + DecisionStore,
{
    let filed = session(FILED)?;
    let backups = topic(store, "Backups", TopicKind::Work, None, 1).await?;
    let jobs = topic(store, "Backup jobs", TopicKind::Work, None, 2).await?;
    let nas = topic(
        store,
        "nas-01",
        TopicKind::Entity,
        Some(hosts(store).await?),
        3,
    )
    .await?;
    store
        .keep_decisions(&[Decision::new(
            backups.id,
            DecisionText::new("Keep 14 days of nightly dumps")?,
            "phil",
            Some(filed.clone()),
            at(4),
        )])
        .await?;
    store
        .assert_facts(&[NewClaim {
            entity: nas.id,
            key: FactKey::new("retention")?,
            value: FactValue::new("14 days")?,
            said: Said::Writer {
                session: filed.clone(),
                summary: Uuid::new_v4(),
                evidence: Evidence::quote("raised retention to 14 days"),
                settled: false,
            },
            owner: Some(crate::owner()?),
            at: at(5),
        }])
        .await?;
    store
        .place(Placement {
            session: filed.clone(),
            topic: backups.id,
            by: PlacementSource::Categorizer,
            at: at(6),
            owner: Some(crate::owner()?),
        })
        .await?;
    store.merge(jobs.id, backups.id, at(7)).await?;
    Ok((filed, nas.id))
}

pub async fn changes_list_what_the_day_recorded<S>(store: &S) -> Outcome
where
    S: TopicStore + FactStore + DecisionStore + ChangeFeed,
{
    let (filed, nas) = seed(store).await?;
    let source = Some(filed.to_string());
    let day = store.changes(&ChangeWindow::between(at(0), at(60))).await?;
    same(
        "every record of the window, newest first",
        shown(&day),
        vec![
            (
                ChangeKind::Merge,
                "Merged Backup jobs into Backups.".to_owned(),
                None,
            ),
            (
                ChangeKind::Filed,
                "Filed under Backups.".to_owned(),
                source.clone(),
            ),
            (
                ChangeKind::Fact,
                "retention: 14 days".to_owned(),
                source.clone(),
            ),
            (
                ChangeKind::Decision,
                "Keep 14 days of nightly dumps".to_owned(),
                source.clone(),
            ),
            (ChangeKind::New, "New entity nas-01.".to_owned(), None),
            (ChangeKind::New, "New topic Backups.".to_owned(), None),
        ],
    )?;
    same(
        "a filing reads as the categorizer's, a fact as the wiki writer's",
        day.iter().map(|change| change.actor).collect::<Vec<_>>(),
        vec![
            ChangeActor::You,
            ChangeActor::Categorizer,
            ChangeActor::WikiWriter,
            ChangeActor::You,
            ChangeActor::WikiWriter,
            ChangeActor::You,
        ],
    )?;
    same(
        "the window's end is exclusive and its start inclusive",
        shown(&store.changes(&ChangeWindow::between(at(5), at(6))).await?),
        vec![(
            ChangeKind::Fact,
            "retention: 14 days".to_owned(),
            source.clone(),
        )],
    )?;
    let mut caused = ChangeWindow::between(at(0), at(60));
    caused.subject = Some(SubjectKey::Session(filed.clone()));
    same(
        "a session's changes are those it is the source of",
        shown(&store.changes(&caused).await?)
            .into_iter()
            .map(|(kind, _, _)| kind)
            .collect::<Vec<_>>(),
        vec![ChangeKind::Filed, ChangeKind::Fact, ChangeKind::Decision],
    )?;
    let mut history = ChangeWindow::between(at(0), at(60));
    history.from = None;
    history.subject = Some(SubjectKey::Topic(nas));
    same(
        "an entity's history has its facts and its creation",
        shown(&store.changes(&history).await?)
            .into_iter()
            .map(|(kind, _, _)| kind)
            .collect::<Vec<_>>(),
        vec![ChangeKind::Fact, ChangeKind::New],
    )?;
    Ok(())
}

pub async fn digests_are_kept_per_day_and_owner<S>(store: &S) -> Outcome
where
    S: DigestStore,
{
    let day = NaiveDate::from_ymd_opt(2026, 9, 21).ok_or("a valid date")?;
    let owner = crate::owner()?;
    let other = Owner::new("someone-else".to_owned())?;
    same("no digest yet", store.digest(day, &owner).await?, None)?;
    let first = DailyDigest {
        day,
        owner: owner.clone(),
        summary: "Work on [Backups](/topics/0000).".to_owned(),
        changes: 3,
        written_at: at(10),
    };
    store.keep_digest(&first).await?;
    same(
        "the digest reads back",
        store.digest(day, &owner).await?,
        Some(first.clone()),
    )?;
    let second = DailyDigest {
        summary: "More work on backups.".to_owned(),
        changes: 5,
        written_at: at(20),
        ..first
    };
    store.keep_digest(&second).await?;
    same(
        "a newer digest replaces the day's",
        store.digest(day, &owner).await?,
        Some(second),
    )?;
    same(
        "another owner has none",
        store.digest(day, &other).await?,
        None,
    )?;
    store
        .request_digest(&DigestRequest::new(day, at(0), at(60), &owner))
        .await?;
    store
        .request_digest(&DigestRequest::new(day, at(0), at(60), &owner))
        .await?;
    Ok(())
}

pub async fn a_tag_on_a_bare_claude_conversation_is_its_sessions_change<S>(store: &S) -> Outcome
where
    S: MemoryStore + ChangeFeed,
{
    const BARE: &str = "00000000-0000-4000-8000-0000000000c7";
    let conversation = crate::filing::capture(store, BARE, "claude-code", "Bare", None).await?;
    let elsewhere = "00000000-0000-4000-8000-0000000000c8";
    let other = crate::filing::capture(store, elsewhere, "cli", "Elsewhere", None).await?;
    for entity in [conversation, other] {
        store
            .add_tag(TagAdd {
                entity_type: TagEntityType::Conversation,
                entity: TagEntity::Id(entity),
                agent: None,
                tag: "area:storage".parse()?,
                display: None,
                reason: None,
                decided_by: None,
            })
            .await?;
    }
    let changes = store
        .changes(&ChangeWindow::between(
            at(0),
            Utc::now() + TimeDelta::days(1),
        ))
        .await?;
    same(
        "a bare Claude Code key is its claude-code session; a bare key on another channel names none",
        shown(&changes)
            .into_iter()
            .filter(|(kind, _, _)| *kind == ChangeKind::Tag)
            .collect::<Vec<_>>(),
        vec![(
            ChangeKind::Tag,
            "Tagged area: storage.".to_owned(),
            Some(format!("claude-code:{BARE}")),
        )],
    )?;
    Ok(())
}
