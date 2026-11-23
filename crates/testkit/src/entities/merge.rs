use kernel::topics::TopicId;
use kernel::wiki::entity_pass::{AppliedMark, CandidateQuery};

use super::{EntityScenarioStore, at, commit, conversation, leaf_at, rewrite};
use crate::topics::Outcome;

pub async fn oldest_survives(store: &impl EntityScenarioStore) -> Outcome {
    let conversation = conversation(store).await?;
    let older = leaf_at(store, conversation, "first", at("2026-10-01T00:00:00Z")?).await?;
    let newer = leaf_at(store, conversation, "second", at("2026-11-01T00:00:00Z")?).await?;
    let keep = commit(store, &older, "box-a", "Hosts", &[], None).await?;
    let drop = commit(store, &newer, "G9", "Hosts", &["box9"], None).await?;
    rewrite::apply(
        store,
        keep.id,
        &rewrite::body(&older, "Older host", &[("ram", "87 GB")], "Older history."),
    )
    .await?;
    rewrite::apply(
        store,
        drop.id,
        &rewrite::body(&newer, "Newer host", &[("ram", "152 GB")], "Newer history."),
    )
    .await?;
    let mut probe = vec![0.0; 1024];
    *probe.first_mut().ok_or("missing dimension")? = 1.0;
    let older_vector = probe.iter().map(|value| -*value).collect::<Vec<_>>();
    store
        .remember_embedding(keep.id, &older_vector, older.start)
        .await?;
    store
        .remember_embedding(drop.id, &probe, newer.start)
        .await?;
    let result = store
        .merge_entities(drop.id, keep.id, newer.start)
        .await?
        .ok_or("merge missing")?;
    assert!(result.performed);
    assert_eq!((result.survivor, result.absorbed), (keep.id, drop.id));
    assert_eq!(
        (result.survivor_name.as_str(), result.absorbed_name.as_str()),
        ("box-a", "G9")
    );
    assert_survivor(store, keep.id, drop.id, &newer).await?;
    let found = store
        .match_candidates(&CandidateQuery {
            names: vec!["box9".to_owned()],
            type_name: "Hosts".to_owned(),
            embedding: Some(probe),
            markers: Vec::new(),
        })
        .await?;
    let candidate = found.first().ok_or("survivor was not found")?;
    assert_eq!(candidate.id, keep.id);
    assert!(candidate.aliases.contains(&"G9".to_owned()));
    assert!((candidate.embedding - 1.0).abs() < f32::EPSILON);
    let old_slug = kernel::wiki::page::WikiSlug::try_from(drop.slug.clone())?;
    assert_eq!(
        store.wiki_page(&old_slug).await?,
        kernel::wiki::WikiLookup::Redirect(keep.slug.clone())
    );
    let again = store
        .merge_entities(drop.id, keep.id, newer.start)
        .await?
        .ok_or("repeat merge missing")?;
    assert!(!again.performed);
    assert_eq!(again.survivor, keep.id);
    Ok(())
}

async fn assert_survivor(
    store: &impl EntityScenarioStore,
    keep: uuid::Uuid,
    drop: uuid::Uuid,
    newer: &kernel::wiki::entity_pass::EntityLeaf,
) -> Outcome {
    let mut lease = store.lease_entity(keep).await?;
    let page = lease.page().await?;
    assert_eq!(page.summary, "Older host");
    assert_eq!(
        page.facts.first().map(|fact| fact.value.as_str()),
        Some("152 GB")
    );
    assert_eq!(
        lease.applied_through().await?,
        Some(AppliedMark {
            at: newer.start,
            summary_id: newer.id
        })
    );
    assert_eq!(page.observed_types, ["Hosts", "Hosts"]);
    lease.release().await?;
    let history = store.entity_history(TopicId::from_uuid(keep)).await?;
    assert_eq!(
        history
            .iter()
            .map(|line| line.sentence.as_str())
            .collect::<Vec<_>>(),
        ["Newer history.", "Older history."]
    );
    assert_eq!(store.claims(TopicId::from_uuid(keep)).await?.len(), 2);
    assert_eq!(store.claims(TopicId::from_uuid(drop)).await?, Vec::new());
    Ok(())
}

pub async fn binds_unbound_survivor(
    store: &(impl EntityScenarioStore + kernel::wiki::builder::WikiBuilderStore),
) -> Outcome {
    use kernel::wiki::WikiLookup;
    use kernel::wiki::page::WikiSlug;

    let conversation = conversation(store).await?;
    let older = leaf_at(store, conversation, "older", at("2026-10-01T00:00:00Z")?).await?;
    let newer = leaf_at(store, conversation, "newer", at("2026-11-01T00:00:00Z")?).await?;
    let keep = commit(store, &older, "box-a", "Hosts", &[], None).await?;
    let drop = commit(store, &newer, "G9", "Hosts", &[], None).await?;
    let slug = WikiSlug::try_from(keep.slug)?;
    seed_unbound_page(store, &older, &slug).await?;
    let WikiLookup::Page(before) = store.wiki_page(&slug).await? else {
        return Err("unbound wiki page missing".into());
    };
    assert_eq!(before.topic_id, None);
    let merged = store
        .merge_entities(drop.id, keep.id, newer.start)
        .await?
        .ok_or("merge missing")?;
    assert!(merged.performed);
    assert_eq!(merged.survivor, keep.id);
    let WikiLookup::Page(after) = store.wiki_page(&slug).await? else {
        return Err("survivor wiki page missing".into());
    };
    assert_eq!(after.topic_id, Some(keep.id.to_string()));
    assert_eq!(after.current_state, before.current_state);
    Ok(())
}

async fn seed_unbound_page(
    store: &impl kernel::wiki::builder::WikiBuilderStore,
    leaf: &kernel::wiki::entity_pass::EntityLeaf,
    slug: &kernel::wiki::page::WikiSlug,
) -> Outcome {
    use kernel::wiki::builder::{ExtractWiki, WikiRouting};
    use kernel::wiki::page::{WikiAction, WikiPatch};

    let patch = WikiPatch {
        action: WikiAction::Create,
        slug: slug.clone(),
        title: "The older host".to_owned(),
        aliases: Vec::new(),
        tags: Vec::new(),
        entities: Vec::new(),
        summary: "The host supports builds.".to_owned(),
        article: None,
        related: None,
        history_entry: None,
        entity_type: None,
        facts: None,
    };
    store
        .apply_wiki(
            &ExtractWiki {
                summary_id: leaf.id,
                conversation_id: leaf.conversation_id,
            },
            vec![patch],
            &WikiRouting::default(),
            leaf.start,
        )
        .await?;
    Ok(())
}
