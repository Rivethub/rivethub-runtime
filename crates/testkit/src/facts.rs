use kernel::facts::Evidence;
use kernel::facts::FactKey;
use kernel::facts::FactState;
use kernel::facts::FactStore;
use kernel::facts::FactValue;
use kernel::facts::NewClaim;
use kernel::facts::Said;
use kernel::facts::SessionLink;
use kernel::facts::facts_as_of;
use kernel::memory::MemoryStore;
use kernel::topics::NewTopic;
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

const FIRST: &str = "00000000-0000-4000-8000-0000000000f1";
const SECOND: &str = "00000000-0000-4000-8000-0000000000f2";

async fn entity(
    store: &dyn TopicStore,
    name: &str,
    kind: &str,
) -> Result<Topic, Box<dyn std::error::Error>> {
    let parent = store
        .list()
        .await?
        .into_iter()
        .find(|listing| {
            listing.topic.kind == TopicKind::Type && listing.topic.name.as_str() == kind
        })
        .ok_or("the seeded type is missing")?
        .topic
        .id;
    let topic = Topic::create(
        TopicId::random(),
        NewTopic {
            kind: TopicKind::Entity,
            name: TopicName::new(name)?,
            aliases: Vec::new(),
            parent: Some(parent),
            directory: None,
            owner: Some(crate::owner()?),
        },
        at(0),
    )?;
    Ok(store.create(topic).await?)
}

fn writer(
    entity: &Topic,
    key: &str,
    value: &str,
    from: &SessionId,
    summary: Uuid,
    minute: u32,
) -> Result<NewClaim, Box<dyn std::error::Error>> {
    Ok(NewClaim {
        entity: entity.id,
        key: FactKey::new(key)?,
        value: FactValue::new(value)?,
        said: Said::Writer {
            session: from.clone(),
            summary,
            evidence: Evidence::quote(&format!("the {key} is {value}")),
            settled: false,
        },
        owner: Some(crate::owner()?),
        at: at(minute),
    })
}

fn you(
    entity: &Topic,
    key: &str,
    value: &str,
    pinned: bool,
    minute: u32,
) -> Result<NewClaim, Box<dyn std::error::Error>> {
    Ok(NewClaim {
        entity: entity.id,
        key: FactKey::new(key)?,
        value: FactValue::new(value)?,
        said: Said::You {
            by: crate::owner()?,
            pinned,
        },
        owner: None,
        at: at(minute),
    })
}

fn shown(facts: &[FactState]) -> Vec<(&str, &str, bool, Vec<&str>)> {
    facts
        .iter()
        .map(|fact| {
            (
                fact.key.as_str(),
                fact.value.as_str(),
                fact.pinned,
                fact.candidates
                    .iter()
                    .map(|candidate| candidate.value.as_str())
                    .collect(),
            )
        })
        .collect()
}

pub async fn facts_keep_claims_pins_and_merges<S>(store: &S) -> Outcome
where
    S: TopicStore + FactStore + MemoryStore,
{
    let host = entity(store, "web01", "Hosts").await?;
    let person = entity(store, "Ana", "People").await?;
    let (first, second) = (session(FIRST)?, session(SECOND)?);
    let (one, two) = (Uuid::new_v4(), Uuid::new_v4());
    store
        .assert_facts(&[
            writer(&host, "OS", "Ubuntu 22.04", &first, one, 0)?,
            writer(&host, "address", "192.0.2.4", &first, one, 0)?,
            writer(&person, "role", "maintainer", &first, one, 0)?,
        ])
        .await?;
    store
        .assert_facts(&[writer(&host, "os", "Ubuntu 22.04 LTS", &first, one, 0)?])
        .await?;
    same(
        "a summary claims a fact once",
        store.claims(host.id).await?.len(),
        2,
    )?;
    store
        .assert_facts(&[you(&host, "address", "192.0.2.9", true, 5)?])
        .await?;
    store
        .assert_facts(&[writer(&host, "address", "192.0.2.12", &second, two, 10)?])
        .await?;
    let facts = facts_as_of(&store.claims(host.id).await?, None);
    same(
        "the pin holds and the newer source is a candidate",
        shown(&facts),
        vec![
            ("address", "192.0.2.9", true, vec!["192.0.2.12"]),
            ("os", "Ubuntu 22.04 LTS", false, Vec::new()),
        ],
    )?;
    let os = facts
        .iter()
        .find(|fact| fact.key.as_str() == "os")
        .ok_or("no os fact")?;
    same(
        "the writer's claim is its session's",
        os.sources
            .iter()
            .map(|source| &source.session)
            .collect::<Vec<_>>(),
        vec![&first],
    )?;
    let mut links = vec![
        SessionLink {
            entity: host.id,
            session: first.clone(),
        },
        SessionLink {
            entity: host.id,
            session: second.clone(),
        },
        SessionLink {
            entity: person.id,
            session: first.clone(),
        },
    ];
    links.sort();
    let mut stored = store.session_links().await?;
    stored.sort();
    same("sessions link entities", stored, links)?;
    crate::filing::capture(store, first.as_str(), "claude-code", "Set up web01", None).await?;
    let sessions = store
        .fact_sessions(&[first.clone(), second.clone()])
        .await?;
    same(
        "a captured session has its title and messages",
        sessions
            .iter()
            .map(|found| (&found.session, found.title.as_deref(), found.messages))
            .collect::<Vec<_>>(),
        vec![(&first, Some("Set up web01"), 1)],
    )?;
    merged_and_deleted(store, &host, os, &second, two).await
}

async fn merged_and_deleted<S>(
    store: &S,
    host: &Topic,
    os: &FactState,
    second: &SessionId,
    two: Uuid,
) -> Outcome
where
    S: TopicStore + FactStore + MemoryStore,
{
    let twin = entity(store, "web-01 box", "Hosts").await?;
    store
        .assert_facts(&[writer(&twin, "os", "Ubuntu 24.04", second, two, 3000)?])
        .await?;
    store.merge(twin.id, host.id, at(3001)).await?;
    let merged = facts_as_of(&store.claims(host.id).await?, None);
    let merged_os = merged
        .iter()
        .find(|fact| fact.key.as_str() == "os")
        .ok_or("no os fact after the merge")?;
    same("a merge joins a shared key", merged.len(), 2)?;
    same("the older row names the fact", merged_os.id, os.id)?;
    same(
        "the newer claim wins",
        merged_os.value.as_str(),
        "Ubuntu 24.04",
    )?;
    store.delete(host.id).await?;
    same(
        "deleting the entity deletes its facts",
        store.claims(host.id).await?.len(),
        0,
    )?;
    Ok(())
}
