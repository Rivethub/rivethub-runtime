use kernel::wiki::entity_match::{MATCH_THRESHOLD, chosen_id, gate, rank_candidates};
use kernel::wiki::entity_pass::{
    CandidateQuery, EntityProse, MatchDecision, PreparedFact, PublishOutcome,
};
use uuid::Uuid;

use super::{EntityScenarioStore, ScenarioResult, at, commit, conversation, leaf_at, prepared};
use crate::topics::Outcome;

pub async fn new_kind(store: &impl EntityScenarioStore) -> Outcome {
    let leaf = fixture_leaf(store).await?;
    let fleet = commit(store, &leaf, "north", "Fleets", &[], None).await?;
    assert_eq!(fleet.type_name, "Fleets");
    assert_eq!(fleet.type_slug, "fleets");
    let host = commit(store, &leaf, "box", "host", &[], None).await?;
    assert_eq!(host.type_name, "Hosts");
    assert_eq!(host.type_slug, "hosts");
    assert!(
        store
            .type_briefs()
            .await?
            .iter()
            .all(|brief| brief.slug != "host")
    );
    Ok(())
}

pub async fn name_pairs(store: &impl EntityScenarioStore) -> Outcome {
    let leaf = fixture_leaf(store).await?;
    let pairs = [
        (
            "Hugging Face",
            "Organizations",
            "Hugging Face",
            "Software",
            true,
        ),
        ("datahub", "Hosts", "datahub", "Software", true),
        ("rivet-glm", "Agents", "rivet-glm", "Software", true),
        ("rivet-claude", "Agents", "rivet-claude", "Hosts", true),
        ("T3 Code", "Software", "T3 Code", "Projects", true),
        ("Grok", "Agents", "Grok", "Software", true),
        (
            "transformers",
            "Libraries and frameworks",
            "transformers",
            "Software",
            true,
        ),
        ("omarchy", "Hosts", "Omarchy", "Software", true),
        (
            "windowsxp811203",
            "People",
            "windowsxp811203",
            "Organizations",
            false,
        ),
    ];
    for (left_name, left_type, right_name, right_type, same) in pairs {
        let first = commit(store, &leaf, left_name, left_type, &[], None).await?;
        assert_eq!(
            contains(store, &[right_name.to_owned()], right_type, first.id).await?,
            same,
            "{left_name} [{left_type}]"
        );
        let second = commit(store, &leaf, right_name, right_type, &[], None).await?;
        assert_eq!(second.id == first.id, same, "{left_name} [{left_type}]");
        if same {
            assert_eq!(second.type_name, left_type, "{left_name} changed type");
        }
    }
    let glm = commit(store, &leaf, "rivet-glm", "Hosts", &[], None).await?;
    let glm_software = commit(store, &leaf, "rivet-glm", "Software", &[], None).await?;
    assert_eq!(glm.id, glm_software.id);
    let grok = commit(store, &leaf, "Grok", "Models", &[], None).await?;
    let grok_agent = commit(store, &leaf, "Grok", "Agents", &[], None).await?;
    assert_eq!(grok.id, grok_agent.id);

    let host = commit(store, &leaf, "sparehub", "Hosts", &[], None).await?;
    let bound = commit(store, &leaf, "Spare Hub", "Services", &[], Some(host.id)).await?;
    assert_eq!(bound.id, host.id);
    assert_eq!(bound.type_name, "Hosts");
    assert!(bound.aliases.iter().any(|alias| alias == "Spare Hub"));
    Ok(())
}

pub async fn repo_tails(store: &impl EntityScenarioStore) -> Outcome {
    let leaf = fixture_leaf(store).await?;
    let repo = commit(store, &leaf, "acme/datahub", "Repos", &[], None).await?;
    assert!(contains(store, &["datahub".to_owned()], "Services", repo.id).await?);
    let bound = commit(store, &leaf, "datahub", "Services", &[], None).await?;
    assert_eq!(bound.id, repo.id);
    assert_eq!(bound.type_name, "Repos");

    let long = "bottlecapai/ThinkingCap-Qwen3.8-Flash-Next";
    let model_repo = commit(store, &leaf, long, "Repos", &[], None).await?;
    assert!(
        contains(store, &["ThinkingCap".to_owned()], "Models", model_repo.id).await?,
        "ThinkingCap is a fuzzy neighbour of the longer repo name"
    );
    let thinking = commit(store, &leaf, "ThinkingCap", "Models", &[], None).await?;
    assert_ne!(
        thinking.id, model_repo.id,
        "a short token is not the repo tail, so commit does not merge them"
    );

    let web = commit(store, &leaf, "web", "Services", &[], None).await?;
    let owned = commit(store, &leaf, "acme/web", "Repos", &[], None).await?;
    assert_ne!(owned.id, web.id);
    Ok(())
}

pub async fn concurrent(store: &impl EntityScenarioStore) -> Outcome {
    let left_leaf = fixture_leaf(store).await?;
    let right_leaf = fixture_leaf(store).await?;
    let (left, right) = tokio::join!(
        commit(
            store,
            &left_leaf,
            "Hugging Face",
            "Organizations",
            &[],
            None
        ),
        commit(store, &right_leaf, "Hugging Face", "Software", &[], None),
    );
    let left = left?;
    let right = right?;
    assert_eq!(left.id, right.id);
    let again = commit(store, &left_leaf, "huggingface", "Projects", &[], None).await?;
    assert_eq!(again.id, left.id);
    Ok(())
}

pub async fn rejected_alias(store: &impl EntityScenarioStore) -> Outcome {
    let leaf = fixture_leaf(store).await?;
    let nvidia = commit(store, &leaf, "nvidia", "Organizations", &[], None).await?;
    let card = commit(store, &leaf, "V100", "Hardware", &["nvidia"], None).await?;
    assert_ne!(card.id, nvidia.id);
    assert!(
        card.aliases.iter().all(|alias| alias != "nvidia"),
        "V100 claimed nvidia: {:?}",
        card.aliases
    );
    Ok(())
}

async fn fixture_leaf(
    store: &impl EntityScenarioStore,
) -> ScenarioResult<kernel::wiki::entity_pass::EntityLeaf> {
    let id = conversation(store).await?;
    leaf_at(store, id, "Matching fixture", at("2026-11-13T00:00:00Z")?).await
}

async fn contains(
    store: &impl EntityScenarioStore,
    names: &[String],
    kind: &str,
    id: Uuid,
) -> ScenarioResult<bool> {
    Ok(store
        .match_candidates(&CandidateQuery {
            names: names.to_vec(),
            type_name: kind.to_owned(),
            embedding: None,
            markers: Vec::new(),
        })
        .await?
        .iter()
        .any(|item| item.id == id))
}

pub async fn markers_and_type_aliases(store: &impl EntityScenarioStore) -> Outcome {
    let leaf = fixture_leaf(store).await?;
    let mut prepared = prepared("hally", "Hosts", &["G9"], None);
    prepared.facts.push(PreparedFact {
        summary_id: leaf.id,
        attribute: "hostname".to_owned(),
        value: "box9".to_owned(),
        as_of: leaf.start,
        status: kernel::wiki::entity_pass::FactState::Current,
        conversation_id: leaf.conversation_id,
    });
    let hally = store
        .commit(std::slice::from_ref(&leaf), &prepared, leaf.start)
        .await?
        .ok_or("entity was skipped")?;
    let prose = EntityProse {
        summary: "box9 is this machine".to_owned(),
        article: "also called G9".to_owned(),
    };
    assert!(matches!(
        store.publish(&hally, &prose, leaf.start).await?,
        PublishOutcome::Stored { .. }
    ));
    let found = store
        .match_candidates(&CandidateQuery {
            names: vec!["unrelated".to_owned()],
            type_name: "Hosts".to_owned(),
            embedding: None,
            markers: vec!["box9".to_owned()],
        })
        .await?;
    let hit = found
        .iter()
        .find(|item| item.id == hally.id)
        .ok_or("box9 did not find hally")?;
    assert!((hit.trigram - 1.0).abs() < f32::EPSILON);
    assert!(hit.summary.contains("box9 is this machine"));
    assert!(contains(store, &["G9".to_owned()], "Hosts", hally.id).await?);
    store.remember_type_alias("Hosts", "machine").await?;
    store.remember_type_alias("Hosts", "machine").await?;
    let hosts = store
        .type_briefs()
        .await?
        .into_iter()
        .find(|item| item.name == "Hosts")
        .ok_or("Hosts type missing")?;
    assert_eq!(
        hosts
            .aliases
            .iter()
            .filter(|alias| alias.as_str() == "machine")
            .count(),
        1
    );
    assert!(hosts.examples.contains(&"hally".to_owned()));
    store
        .record_decision(
            leaf.id,
            &MatchDecision {
                kind: "type".to_owned(),
                proposed_name: "hally".to_owned(),
                proposed_type: "machine".to_owned(),
                candidates: vec!["Hosts".to_owned()],
                verdict: "same".to_owned(),
                choice: "Hosts".to_owned(),
                confidence: 0.96,
                model: "fixture".to_owned(),
                entity_id: Some(hally.id),
            },
            leaf.start,
        )
        .await?;
    store
        .hold_candidate(leaf.id, &prepared, 0.4, "ambiguous", leaf.start)
        .await?;
    Ok(())
}

pub async fn matching_scores(store: &impl EntityScenarioStore) -> Outcome {
    let leaf = fixture_leaf(store).await?;
    let extent = commit(store, &leaf, "Two words", "Hosts", &[], None).await?;
    let alias = commit(store, &leaf, "Elsewhere", "Hosts", &["Wordsome"], None).await?;
    let other = commit(store, &leaf, "Kestrel", "Hosts", &[], None).await?;
    let mut vector = vec![0.0; 1024];
    let mut first = vector.iter_mut();
    *first.next().ok_or("first vector coordinate missing")? = 3.0;
    *first.next().ok_or("second vector coordinate missing")? = 4.0;
    store
        .remember_embedding(extent.id, &vector, leaf.start)
        .await?;
    store
        .remember_embedding(
            alias.id,
            &vector.iter().map(|value| -*value).collect::<Vec<_>>(),
            leaf.start,
        )
        .await?;
    let mut probe = vec![0.0; 1024];
    *probe.first_mut().ok_or("vector coordinate missing")? = 1.0;
    let found = store
        .match_candidates(&CandidateQuery {
            names: vec![" word ".to_owned()],
            type_name: "Hosts".to_owned(),
            embedding: Some(probe),
            markers: Vec::new(),
        })
        .await?;
    let locate = |id| {
        found
            .iter()
            .find(|hit| hit.id == id)
            .ok_or("candidate missing")
    };
    let extent = locate(extent.id)?;
    let alias = locate(alias.id)?;
    let other = locate(other.id)?;
    assert!((extent.trigram - 0.8).abs() < 0.000_001, "{extent:?}");
    assert!((alias.trigram - 0.8).abs() < 0.000_001, "{alias:?}");
    assert!((extent.embedding - 0.6).abs() < 0.000_001, "{extent:?}");
    assert!(alias.embedding.abs() < f32::EPSILON);
    assert!(other.trigram.abs() < f32::EPSILON);
    assert_eq!(alias.aliases, ["Elsewhere", "Wordsome"]);
    let expected = extent.id;
    let ranked = rank_candidates(found);
    let choice = ranked
        .iter()
        .position(|item| item.id == expected)
        .ok_or("ranked candidate missing")?;
    let decision = gate(&choice.to_string(), 0.95, MATCH_THRESHOLD, ranked.len());
    assert_eq!(chosen_id("Hosts", &ranked, decision), Some(expected));
    accented_extent(store, &leaf).await
}

async fn accented_extent(
    store: &impl EntityScenarioStore,
    leaf: &kernel::wiki::entity_pass::EntityLeaf,
) -> Outcome {
    let entity = commit(store, leaf, "Caféine", "Software", &[], None).await?;
    let found = store
        .match_candidates(&CandidateQuery {
            names: vec!["CAFÉ".to_owned()],
            type_name: "Software".to_owned(),
            embedding: None,
            markers: Vec::new(),
        })
        .await?;
    let found = found
        .iter()
        .find(|item| item.id == entity.id)
        .ok_or("accented candidate missing")?;
    assert!((found.trigram - 0.8).abs() < 0.000_001);
    Ok(())
}
