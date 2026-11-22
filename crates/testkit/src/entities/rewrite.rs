use kernel::facts::{FactKey, FactValue, NewClaim, Said};
use kernel::topics::TopicId;
use kernel::wiki::entity_pass::{
    AppliedMark, EntityLeaf, FactState, MentionNote, PreparedFact, StoredRewrite,
};
use uuid::Uuid;

use super::{EntityScenarioStore, ScenarioResult, at, commit, conversation, history, leaf_at};
use crate::topics::Outcome;

pub(super) fn body(
    leaf: &EntityLeaf,
    summary: &str,
    facts: &[(&str, &str)],
    sentence: &str,
) -> StoredRewrite {
    let as_of = leaf.start;
    StoredRewrite {
        summary: summary.to_owned(),
        article: format!("{summary} More about this host."),
        facts: facts
            .iter()
            .map(|(attribute, value)| PreparedFact {
                attribute: (*attribute).to_owned(),
                value: (*value).to_owned(),
                as_of,
                status: FactState::Current,
                summary_id: leaf.id,
                conversation_id: leaf.conversation_id,
            })
            .collect(),
        history: Some(history(leaf, sentence)),
        owner: crate::OWNER.to_owned(),
        session_at: leaf.start,
        session_id: leaf.id,
        evidence: sentence.to_owned(),
    }
}

pub(super) async fn apply(
    store: &impl EntityScenarioStore,
    entity: Uuid,
    body: &StoredRewrite,
) -> Outcome {
    let mut lease = store.lease_entity(entity).await?;
    lease.apply(body, body.session_at).await?;
    lease.release().await?;
    Ok(())
}

async fn fixture(
    store: &impl EntityScenarioStore,
) -> ScenarioResult<(EntityLeaf, EntityLeaf, Uuid)> {
    let session = conversation(store).await?;
    let older = leaf_at(store, session, "before", at("2026-11-01T00:00:00Z")?).await?;
    let newer = leaf_at(store, session, "after", at("2026-11-07T00:00:00Z")?).await?;
    let entity = commit(store, &older, "tinbox", "Hosts", &[], None).await?;
    Ok((older, newer, entity.id))
}

pub async fn replacement_and_mention(store: &impl EntityScenarioStore) -> Outcome {
    let (older, newer, entity) = fixture(store).await?;
    let earlier = body(
        &older,
        "GPU host.",
        &[("ram", "87 GB available"), ("ip", "11.22.33.44")],
        "Initial facts.",
    );
    let first = body(
        &newer,
        "GPU host.",
        &[("ram", "152 GB available")],
        "Recorded 152 GB available.",
    );
    let second = body(
        &newer,
        "GPU host.",
        &[("ram", "160 GB available")],
        "Recorded 160 GB available.",
    );
    apply(store, entity, &earlier).await?;
    apply(store, entity, &first).await?;
    apply(store, entity, &second).await?;
    let id = TopicId::from_uuid(entity);
    let before = store.claims(id).await?;
    assert_eq!(before.len(), 3);
    let later = leaf_at(
        store,
        older.conversation_id.ok_or("conversation missing")?,
        "mention",
        at("2026-11-08T00:00:00Z")?,
    )
    .await?;
    let mut lease = store.lease_entity(entity).await?;
    assert!(
        lease
            .record_mention(
                &MentionNote {
                    history: Some(history(&later, "Seen again.")),
                    session_at: later.start,
                    session_id: later.id,
                    owner: later.owner.clone().ok_or("owner missing")?,
                },
                later.start
            )
            .await?
            .is_some()
    );
    assert_eq!(
        lease.applied_through().await?,
        Some(AppliedMark {
            at: later.start,
            summary_id: later.id
        })
    );
    let page = lease.page().await?;
    assert_eq!(page.summary, "GPU host.");
    assert_eq!(
        page.facts
            .iter()
            .map(|fact| (fact.attribute.as_str(), fact.value.as_str()))
            .collect::<Vec<_>>(),
        [("ip", "11.22.33.44"), ("ram", "160 GB available")]
    );
    lease.release().await?;
    assert_eq!(store.claims(id).await?, before);
    let history = store.entity_history(id).await?;
    assert_eq!(
        history
            .iter()
            .map(|line| line.sentence.as_str())
            .collect::<Vec<_>>(),
        [
            "Seen again.",
            "Recorded 160 GB available.",
            "Initial facts."
        ]
    );
    assert_wiki_page(store, entity).await
}

async fn assert_wiki_page(store: &impl EntityScenarioStore, entity: Uuid) -> Outcome {
    let slug = kernel::wiki::page::WikiSlug::try_from("tinbox".to_owned())?;
    let kernel::wiki::WikiLookup::Page(page) = store.wiki_page(&slug).await? else {
        return Err("entity wiki page missing".into());
    };
    assert_eq!(page.topic_id.as_deref(), Some(entity.to_string().as_str()));
    assert!(page.markdown.contains("Seen again."));
    assert!(page.markdown.contains("Recorded 160 GB available."));
    Ok(())
}

pub async fn out_of_order(store: &impl EntityScenarioStore) -> Outcome {
    ordered(store, false).await
}

pub async fn concurrent(store: &impl EntityScenarioStore) -> Outcome {
    ordered(store, true).await
}

async fn ordered(store: &impl EntityScenarioStore, concurrent: bool) -> Outcome {
    let (older, newer, entity) = fixture(store).await?;
    let later = body(
        &newer,
        "Newer host.",
        &[("ram", "152 GB available")],
        "Recorded 152 GB available.",
    );
    let earlier = body(
        &older,
        "Older host.",
        &[("ram", "87 GB available"), ("ip", "11.22.33.44")],
        "Recorded the address.",
    );
    if concurrent {
        tokio::try_join!(apply(store, entity, &later), apply(store, entity, &earlier))?;
    } else {
        apply(store, entity, &later).await?;
        apply(store, entity, &earlier).await?;
    }
    let mut lease = store.lease_entity(entity).await?;
    let page = lease.page().await?;
    assert_eq!(page.summary, "Newer host.");
    assert_eq!(
        page.facts
            .iter()
            .map(|fact| (fact.attribute.as_str(), fact.value.as_str()))
            .collect::<Vec<_>>(),
        [("ip", "11.22.33.44"), ("ram", "152 GB available")]
    );
    assert_eq!(
        lease.applied_through().await?,
        Some(AppliedMark {
            at: newer.start,
            summary_id: newer.id
        })
    );
    lease.release().await?;
    let history = store.entity_history(TopicId::from_uuid(entity)).await?;
    assert_eq!(
        history
            .iter()
            .map(|line| line.sentence.as_str())
            .collect::<Vec<_>>(),
        ["Recorded 152 GB available.", "Recorded the address."]
    );
    Ok(())
}

pub async fn restated_fact(store: &impl EntityScenarioStore) -> Outcome {
    let (older, newer, entity) = fixture(store).await?;
    let id = TopicId::from_uuid(entity);
    let owner = crate::owner()?;
    store
        .assert_facts(&[NewClaim {
            entity: id,
            key: FactKey::new("role")?,
            value: FactValue::new("compute")?,
            said: Said::You {
                by: owner.clone(),
                pinned: true,
            },
            owner: Some(owner),
            at: older.start,
        }])
        .await?;
    let first = body(
        &older,
        "GPU host.",
        &[("ram", "87 GB"), ("role", "other")],
        "First mention.",
    );
    let restated = body(
        &newer,
        "GPU host.",
        &[("ram", "87 GB"), ("role", "compute")],
        "Again.",
    );
    apply(store, entity, &first).await?;
    apply(store, entity, &restated).await?;
    let claims = store.claims(id).await?;
    assert_eq!(
        claims
            .iter()
            .filter(|claim| claim.key.as_str() == "ram")
            .count(),
        1
    );
    assert_eq!(
        claims
            .iter()
            .filter(|claim| claim.value.as_str() == "compute")
            .count(),
        2
    );
    assert!(
        claims
            .iter()
            .filter(|claim| claim.key.as_str() == "ram")
            .all(|claim| { matches!(claim.said, Said::Writer { settled: true, .. }) })
    );
    let mut lease = store.lease_entity(entity).await?;
    let page = lease.page().await?;
    assert_eq!(page.fixed, ["role"]);
    assert_eq!(
        page.facts
            .iter()
            .find(|fact| fact.attribute == "role")
            .map(|fact| fact.value.as_str()),
        Some("compute")
    );
    lease.release().await?;
    Ok(())
}
