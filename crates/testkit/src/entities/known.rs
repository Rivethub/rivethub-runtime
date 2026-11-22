use jiff::SignedDuration as Duration;
use kernel::wiki::entity_pass::{EntityLeaf, EntityView, KNOWN_LIMIT, KnownEntity, PreparedFact};
use uuid::Uuid;
use wire::time::TimestampFormat as _;

use super::{EntityScenarioStore, ScenarioResult, at, conversation, leaf_at, prepared};
use crate::topics::Outcome;

pub async fn match_case(store: &impl EntityScenarioStore) -> Outcome {
    let datahub = commit(store, "datahub", "Hosts", &[], "2026-11-01T00:00:00Z").await?;
    let aliased = commit(
        store,
        "sparehost",
        "Hosts",
        &["comfyui"],
        "2026-11-01T00:01:00Z",
    )
    .await?;
    let folded = commit(store, "tinbox", "Hosts", &[], "2026-11-01T00:02:00Z").await?;
    let repo = commit(
        store,
        "acme/Widget-Box",
        "Repos",
        &[],
        "2026-11-01T00:03:00Z",
    )
    .await?;
    let alias_folded = commit(
        store,
        "sparepump",
        "Hosts",
        &["Nfs-Server"],
        "2026-11-01T00:04:00Z",
    )
    .await?;
    let absent = commit(
        store,
        "notmentionedxyz",
        "Hosts",
        &[],
        "2026-11-01T00:05:00Z",
    )
    .await?;
    let id = conversation(store).await?;
    let leaf = leaf_at(store, id, "Known entity fact", at("2026-11-01T00:30:00Z")?).await?;
    let mut item = prepared("datahub", "Hosts", &[], Some(datahub.id));
    item.facts.push(PreparedFact {
        attribute: "ip".to_owned(),
        value: "11.22.33.44".to_owned(),
        summary_id: leaf.id,
        as_of: leaf.start,
        status: kernel::wiki::entity_pass::FactState::Current,
        conversation_id: leaf.conversation_id,
    });
    store
        .commit(std::slice::from_ref(&leaf), &item, leaf.end)
        .await?;
    let known = known_in(
        store,
        "Tin-Box hosts datahub beside comfyui. We shipped ACME/widget-box after nfsserver came up.",
    )
    .await?;
    let found = named(&known, "datahub").ok_or("datahub missing")?;
    assert_eq!(found.id, datahub.id);
    assert_eq!(found.type_name, "Hosts");
    assert!(
        found
            .facts
            .iter()
            .any(|fact| { fact.attribute == "ip" && fact.value == "11.22.33.44" })
    );
    let host = named(&known, "sparehost").ok_or("alias missing")?;
    assert_eq!(host.id, aliased.id);
    assert!(host.aliases.iter().any(|alias| alias == "comfyui"));
    assert_eq!(named(&known, "tinbox").map(|item| item.id), Some(folded.id));
    assert_eq!(
        named(&known, "acme/Widget-Box").map(|item| item.id),
        Some(repo.id)
    );
    let pump = named(&known, "sparepump").ok_or("folded alias missing")?;
    assert_eq!(pump.id, alias_folded.id);
    assert!(pump.aliases.iter().any(|alias| alias == "Nfs-Server"));
    assert!(known.iter().all(|item| item.id != absent.id));
    Ok(())
}

pub async fn order_case(store: &impl EntityScenarioStore) -> Outcome {
    commit(store, "beta-two", "Hosts", &[], "2026-11-01T00:00:00Z").await?;
    commit(store, "alphaone", "Hosts", &[], "2026-11-02T00:00:00Z").await?;
    commit(store, "gammaone", "Hosts", &[], "2026-11-03T00:00:00Z").await?;
    let known = known_in(store, "alphaone met gammaone near betatwo").await?;
    let names: Vec<&str> = known.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(names, vec!["alphaone", "gammaone", "beta-two"]);
    Ok(())
}

pub async fn limit_case(store: &impl EntityScenarioStore) -> Outcome {
    let limit = usize::try_from(KNOWN_LIMIT)?;
    let total = limit + 10;
    let mut document = String::new();
    for index in 0..total {
        let name = format!("lim{index:02}");
        document.push_str(&name);
        document.push(' ');
        let when = at("2026-11-01T00:00:00Z")? + Duration::from_secs(i64::try_from(index)?);
        commit(store, &name, "Hosts", &[], &when.rfc3339_offset()).await?;
    }
    let known = known_in(store, &document).await?;
    assert_eq!(known.len(), limit);
    assert_eq!(known.first().map(|item| item.name.as_str()), Some("lim00"));
    let last = format!("lim{index:02}", index = limit - 1);
    assert_eq!(
        known.get(limit - 1).map(|item| item.name.as_str()),
        Some(last.as_str())
    );
    let dropped = format!("lim{index:02}", index = total - 1);
    assert!(known.iter().all(|item| item.name != dropped));
    Ok(())
}

fn named<'a>(known: &'a [KnownEntity], name: &str) -> Option<&'a KnownEntity> {
    known.iter().find(|item| item.name == name)
}

fn leaf(content: &str, start: &str) -> ScenarioResult<EntityLeaf> {
    Ok(EntityLeaf {
        delivery: None,
        id: Uuid::new_v4(),
        conversation_id: None,
        content: content.to_owned(),
        start: at(start)?,
        end: at(start)?,
        owner: Some("owner".to_owned()),
    })
}

async fn known_in(
    store: &impl EntityScenarioStore,
    content: &str,
) -> ScenarioResult<Vec<KnownEntity>> {
    let probe = leaf(content, "2026-11-09T00:00:00Z")?;
    Ok(store
        .observe_context(std::slice::from_ref(&probe))
        .await?
        .known)
}

async fn commit(
    store: &impl EntityScenarioStore,
    name: &str,
    kind: &str,
    aliases: &[&str],
    time: &str,
) -> ScenarioResult<EntityView> {
    let conversation = conversation(store).await?;
    let leaf = leaf_at(store, conversation, name, at(time)?).await?;
    super::commit(store, &leaf, name, kind, aliases, None).await
}
