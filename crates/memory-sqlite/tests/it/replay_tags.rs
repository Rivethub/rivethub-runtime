use crate::common;

use std::collections::BTreeMap;

use chrono::DateTime;
use chrono::Utc;
use common::Ids;
use common::Scenario;
use common::ScenarioFile;
use common::Step;
use common::TestResult;
use common::expect_json;
use common::fresh_store;
use common::instant;
use common::recording;
use kernel::memory::Limit;
use kernel::memory::MemoryStore;
use kernel::memory::TagAdd;
use kernel::memory::TagCountsQuery;
use kernel::memory::TagDecision;
use kernel::memory::TagEntity;
use kernel::memory::TagEntityType;
use kernel::memory::TagEntryPair;
use kernel::memory::TagId;
use kernel::memory::TagListQuery;
use kernel::memory::TagLiteral;
use kernel::memory::TagLookup;
use kernel::memory::TagState;
use kernel::memory::TagSuggestKind;
use kernel::memory::TagSuggestionRow;
use kernel::memory::TagSuggestionWrite;
use kernel::memory::TagTaxonomyDecision;
use kernel::memory::TagTaxonomyEntry;
use kernel::memory::TagTaxonomyMerge;
use kernel::memory::TagTaxonomyQuery;
use kernel::memory::TagTaxonomyUpsert;
use kernel::memory::TagVocabularyProposal;
use memory_sqlite::SqliteStore;
use rusqlite::params;
use serde::Deserialize;
use testkit::Json;
use uuid::Uuid;

const LIFECYCLE: &str = "tag store lifecycle";
const VOCAB: &str = "vocabulary: upsert, nest, decide, propose, merge";
const SESS_1: &str = "00000000-0000-4000-8000-000000000002";
const SESS_2_HELPER: &str = "00000000-0000-4000-8000-000000000005";
const SESS_2_RIVET: &str = "11111111-1111-4111-8111-111111111111";

#[tokio::test]
async fn tag_lifecycle_matches_the_recording() -> TestResult {
    let file: ScenarioFile = recording("tags.json")?;
    let scenario = named(&file, LIFECYCLE)?;
    let fresh = fresh_store()?;
    seed_sessions(&fresh.store).await?;
    let mut ids = Ids::default();
    let acmekit = replay_adds(&fresh.store, scenario, &mut ids).await?;
    replay_lists(&fresh.store, scenario, &mut ids).await?;
    replay_decisions(&fresh.store, scenario, &acmekit, &mut ids).await?;
    compare_saved(&fresh.store, scenario, &mut ids, LIFECYCLE).await
}

#[tokio::test]
async fn vocabulary_matches_the_recording() -> TestResult {
    let file: ScenarioFile = recording("tags.json")?;
    let scenario = named(&file, VOCAB)?;
    let fresh = fresh_store()?;
    let at = instant(common::RECORDED_START)?;
    let store = fresh.store.clone().with_clock(move || at);
    let mut ids = Ids::default();
    replay_topic(&store, scenario, &mut ids).await?;
    replay_cycle(&store, scenario).await?;
    replay_proposals(&store, scenario, &mut ids).await?;
    replay_merges(&store, scenario, &mut ids).await?;
    expect_saved_vocabulary(&store, scenario, &mut ids).await
}

#[tokio::test]
async fn an_add_defaults_a_blank_display_to_the_value() -> TestResult {
    let fresh = fresh_store()?;
    seed_sessions(&fresh.store).await?;

    let omitted = fresh
        .store
        .add_tag(session_add(
            "cli:sess-1",
            "project:acmekit",
            None,
            None,
            None,
            None,
        )?)
        .await?;
    expect_text("acmekit", &omitted.display)?;

    let blank = fresh
        .store
        .add_tag(session_add(
            "cli:sess-1",
            "project:acmekit-tools",
            Some("   "),
            None,
            None,
            None,
        )?)
        .await?;
    expect_text("acmekit-tools", &blank.display)?;
    Ok(())
}

#[tokio::test]
async fn a_session_key_does_not_address_a_summary() -> TestResult {
    let fresh = fresh_store()?;
    let outcome = fresh
        .store
        .add_tag(TagAdd {
            entity_type: TagEntityType::Summary,
            entity: TagEntity::SessionKey("cli:sess-1".to_owned()),
            agent: None,
            tag: TagLiteral::parse_add("topic:notes")?,
            display: None,
            reason: None,
            decided_by: None,
        })
        .await;
    match outcome {
        Err(error) => expect_text(
            "a session key addresses a conversation, not a summary",
            &error.to_string(),
        ),
        Ok(_) => Err("a summary accepted a session key".into()),
    }
}

async fn replay_adds(
    store: &SqliteStore,
    scenario: &Scenario,
    ids: &mut Ids,
) -> TestResult<String> {
    let first = add_ok(
        store,
        step(scenario, "add by session key + literal")?,
        ids,
        session_add(
            "cli:sess-1",
            "project:acmekit",
            Some("AcmeKit"),
            None,
            None,
            None,
        )?,
    )
    .await?;
    add_ok(
        store,
        step(scenario, "add by key/value/display/reason")?,
        ids,
        TagAdd {
            entity_type: TagEntityType::Conversation,
            entity: TagEntity::SessionKey("cli:sess-1".to_owned()),
            agent: None,
            tag: TagLiteral {
                key: "topic".to_owned(),
                value: "blue-green-deploys".to_owned(),
            },
            display: Some("Blue/green".to_owned()),
            reason: Some("manual".to_owned()),
            decided_by: None,
        },
    )
    .await?;
    add_ok(
        store,
        step(scenario, "add same again")?,
        ids,
        session_add(
            "cli:sess-1",
            "project:acmekit",
            Some("AcmeKit"),
            None,
            None,
            Some("other"),
        )?,
    )
    .await?;
    add_err(
        store,
        step(scenario, "add to a session key with two agents (no agent)")?,
        session_add("cli:sess-2", "project:other", None, None, None, None)?,
    )
    .await?;
    add_ok(
        store,
        step(scenario, "add with agent")?,
        ids,
        session_add(
            "cli:sess-2",
            "project:infra",
            None,
            None,
            Some("helper"),
            None,
        )?,
    )
    .await?;
    replay_odd_adds(store, scenario, ids).await?;
    Ok(first)
}

async fn replay_odd_adds(store: &SqliteStore, scenario: &Scenario, ids: &mut Ids) -> TestResult {
    let bad = step(scenario, "add bad literal")?;
    let literal = match TagLiteral::parse_add("nocolon") {
        Err(error) => error.to_string(),
        Ok(_) => return Err("nocolon parsed as a tag".into()),
    };
    expect_text(&bad.error.as_ref().ok_or("bad literal")?.message, &literal)?;
    add_err(
        store,
        step(scenario, "add unknown session")?,
        session_add("cli:none", "project:missing", None, None, None, None)?,
    )
    .await?;
    add_ok(
        store,
        step(scenario, "add key with colon")?,
        ids,
        TagAdd {
            entity_type: TagEntityType::Conversation,
            entity: TagEntity::SessionKey("cli:sess-1".to_owned()),
            agent: None,
            tag: TagLiteral {
                key: "a:b".to_owned(),
                value: "c".to_owned(),
            },
            display: None,
            reason: None,
            decided_by: None,
        },
    )
    .await?;
    add_ok(
        store,
        step(scenario, "add long value")?,
        ids,
        TagAdd {
            entity_type: TagEntityType::Conversation,
            entity: TagEntity::SessionKey("cli:sess-1".to_owned()),
            agent: None,
            tag: TagLiteral {
                key: "k".to_owned(),
                value: "v".repeat(300),
            },
            display: None,
            reason: None,
            decided_by: None,
        },
    )
    .await?;
    let proposed = step(scenario, "propose (model)")?;
    let at = instant(&proposed.at)?;
    let wrote = store
        .store_tag_suggestions(TagSuggestionWrite {
            proposed_by: "fake-chat".to_owned(),
            at,
            rows: vec![TagSuggestionRow {
                kind: TagSuggestKind::Add,
                entity_type: TagEntityType::Conversation,
                entity_id: Uuid::parse_str(SESS_1)?,
                key: "topic".to_owned(),
                value: "rollback".to_owned(),
                display: String::new(),
                confidence: Some(0.8),
                reason: Some("mentioned".to_owned()),
            }],
            vocabulary: Vec::new(),
        })
        .await?;
    expect_json(
        recorded(proposed)?,
        &Json::from(wrote.tags),
        &proposed.label,
        ids,
    )?;
    Ok(())
}

async fn replay_lists(store: &SqliteStore, scenario: &Scenario, ids: &mut Ids) -> TestResult {
    expect_list(
        store,
        step(scenario, "list default")?,
        ids,
        None,
        Vec::new(),
    )
    .await?;
    expect_list(
        store,
        step(scenario, "list key/state")?,
        ids,
        Some("project"),
        vec![TagState::Accepted],
    )
    .await?;
    let pending = step(scenario, "pending")?;
    let rows = store.pending_tags(page()).await?;
    expect_json(
        recorded(pending)?,
        &serde_json::to_value(&rows)?,
        &pending.label,
        ids,
    )?;
    expect_counts(store, step(scenario, "counts")?, ids, None).await?;
    expect_counts(
        store,
        step(scenario, "counts key topic")?,
        ids,
        Some("topic"),
    )
    .await
}

async fn replay_decisions(
    store: &SqliteStore,
    scenario: &Scenario,
    acmekit: &str,
    ids: &mut Ids,
) -> TestResult {
    let reject = step(scenario, "decide reject")?;
    let store = pinned(store, reject)?;
    let changed = store
        .decide_tags(vec![tag_id(acmekit)?], TagDecision::Reject, None)
        .await?;
    expect_id_count(reject, changed, acmekit, ids)?;
    let again = step(scenario, "decide again (no change)")?;
    let store = pinned(&store, again)?;
    let unchanged = store
        .decide_tags(vec![tag_id(acmekit)?], TagDecision::Reject, None)
        .await?;
    expect_id_count(again, unchanged, acmekit, ids)?;
    let lookup = step(scenario, "forSessionKeys")?;
    let found = store
        .lookup_tags(TagLookup {
            session_keys: vec!["cli:sess-1".to_owned(), "cli:sess-2".to_owned()],
            states: Vec::new(),
        })
        .await?;
    expect_json(
        &decode_map(recorded(lookup)?)?,
        &serde_json::to_value(&found)?,
        &lookup.label,
        ids,
    )?;
    expect_counts(&store, step(scenario, "backend tags counts")?, ids, None).await?;
    expect_taxonomy(
        &store,
        step(scenario, "backend taxonomy")?,
        ids,
        None,
        false,
    )
    .await?;
    let upserted = step(scenario, "backend upsertTaxonomy")?;
    let store = pinned(&store, upserted)?;
    let entry = store
        .upsert_tag_taxonomy(vocab(
            "project",
            "Acme App",
            Some("Acme App"),
            None,
            &[],
            None,
        ))
        .await?;
    expect_json(
        recorded(upserted)?,
        &serde_json::to_value(&entry)?,
        &upserted.label,
        ids,
    )?;
    let merge = step(scenario, "backend mergeTaxonomy error")?;
    let store = pinned(&store, merge)?;
    let outcome = store
        .merge_tag_taxonomy(TagTaxonomyMerge {
            key: "k".to_owned(),
            from: "b".to_owned(),
            into: "a".to_owned(),
        })
        .await;
    match outcome {
        Err(error) => expect_text(
            &merge.error.as_ref().ok_or("merge")?.message,
            &error.to_string(),
        ),
        Ok(_) => Err("merge of an unknown value succeeded".into()),
    }
}

async fn replay_topic(store: &SqliteStore, scenario: &Scenario, ids: &mut Ids) -> TestResult {
    upsert_ok(
        store,
        step(scenario, "upsert normalizes")?,
        ids,
        vocab("topic", "Memory Compaction", None, None, &[], None),
    )
    .await?;
    upsert_ok(
        store,
        step(scenario, "upsert aliases (self alias dropped)")?,
        ids,
        vocab("topic", "memory", Some(""), None, &["mem", "memory"], None),
    )
    .await?;
    upsert_ok(
        store,
        step(scenario, "nest")?,
        ids,
        vocab(
            "topic",
            "memory-compaction",
            None,
            Some("memory"),
            &[],
            Some("nested"),
        ),
    )
    .await?;
    expect_taxonomy(
        store,
        step(scenario, "list topic")?,
        ids,
        Some("topic"),
        false,
    )
    .await?;
    upsert_ok(
        store,
        step(scenario, "clear parent")?,
        ids,
        vocab("topic", "memory-compaction", None, None, &[], None),
    )
    .await?;
    upsert_err(
        store,
        step(scenario, "missing parent")?,
        vocab("topic", "memory", None, Some("nope"), &[], None),
    )
    .await?;
    upsert_err(
        store,
        step(scenario, "own parent")?,
        vocab("topic", "memory", None, Some("memory"), &[], None),
    )
    .await
}

async fn replay_cycle(store: &SqliteStore, scenario: &Scenario) -> TestResult {
    store
        .upsert_tag_taxonomy(vocab("topic", "a", Some(""), None, &[], None))
        .await?;
    store
        .upsert_tag_taxonomy(vocab("topic", "b", Some(""), Some("a"), &[], None))
        .await?;
    upsert_err(
        store,
        step(scenario, "cycle")?,
        vocab("topic", "a", Some(""), Some("b"), &[], None),
    )
    .await?;
    upsert_err(
        store,
        step(scenario, "empty key")?,
        vocab("", "memory", None, None, &[], None),
    )
    .await
}

async fn replay_proposals(store: &SqliteStore, scenario: &Scenario, ids: &mut Ids) -> TestResult {
    let proposals = vec![
        vocabulary_proposal("topic", "wiki"),
        vocabulary_proposal("topic", "tags"),
    ];
    expect_wrote(store, step(scenario, "propose")?, ids, &proposals).await?;
    let accept = step(scenario, "decide")?;
    let store = pinned(store, accept)?;
    let changed = store
        .decide_tag_taxonomy(TagTaxonomyDecision {
            entries: vec![TagEntryPair {
                key: "topic".to_owned(),
                value: "wiki".to_owned(),
            }],
            decision: TagDecision::Accept,
            decided_by: None,
        })
        .await?;
    expect_json(recorded(accept)?, &Json::from(changed), &accept.label, ids)?;
    let again = step(scenario, "decide again")?;
    let store = pinned(&store, again)?;
    let unchanged = store
        .decide_tag_taxonomy(TagTaxonomyDecision {
            entries: vec![TagEntryPair {
                key: "topic".to_owned(),
                value: "wiki".to_owned(),
            }],
            decision: TagDecision::Accept,
            decided_by: None,
        })
        .await?;
    expect_json(recorded(again)?, &Json::from(unchanged), &again.label, ids)?;
    expect_wrote(
        &store,
        step(scenario, "propose over decided")?,
        ids,
        &proposals,
    )
    .await
}

async fn replay_merges(store: &SqliteStore, scenario: &Scenario, ids: &mut Ids) -> TestResult {
    store
        .upsert_tag_taxonomy(vocab("project", "acme", Some(""), None, &[], None))
        .await?;
    store
        .upsert_tag_taxonomy(vocab(
            "project",
            "acme-app",
            Some(""),
            None,
            &["acme-old"],
            None,
        ))
        .await?;
    store
        .upsert_tag_taxonomy(vocab("project", "legacy", Some(""), None, &[], None))
        .await?;
    store
        .upsert_tag_taxonomy(vocab(
            "project",
            "acme-child",
            Some(""),
            Some("acme"),
            &[],
            None,
        ))
        .await?;
    seed_merge_tags(store).await?;
    expect_merge(
        store,
        step(scenario, "merge acme-app into acme")?,
        ids,
        "acme-app",
        "acme",
    )
    .await?;
    expect_merge(
        store,
        step(scenario, "merge into an alias")?,
        ids,
        "legacy",
        "acme-old",
    )
    .await?;
    merge_err(store, step(scenario, "merge same")?, "acme", "acme").await?;
    merge_err(store, step(scenario, "merge ghost")?, "ghost", "acme").await?;
    merge_err(
        store,
        step(scenario, "merge into own child")?,
        "acme",
        "acme-child",
    )
    .await?;
    expect_taxonomy(
        store,
        step(scenario, "list all project")?,
        ids,
        Some("project"),
        true,
    )
    .await
}

async fn expect_saved_vocabulary(
    store: &SqliteStore,
    scenario: &Scenario,
    ids: &mut Ids,
) -> TestResult {
    let entries = store.list_tag_taxonomy(taxonomy_query(None)).await?;
    let actual = serde_json::to_value(&entries)?;
    let rows = scenario
        .tables
        .get("ros_tag_taxonomy")
        .ok_or("taxonomy table")?;
    let mut projected = Vec::new();
    for row in rows {
        projected.push(project_taxonomy(row)?);
    }
    expect_by_identity(&Json::Array(projected), &actual, "saved vocabulary", ids)
}

async fn seed_sessions(store: &SqliteStore) -> TestResult {
    let rows = [
        (SESS_1, "cli:sess-1", "rivet"),
        (SESS_2_RIVET, "cli:sess-2", "rivet"),
        (SESS_2_HELPER, "cli:sess-2", "helper"),
    ];
    store
        .with_conn(move |conn| {
            for (id, session, agent) in rows {
                conn.execute(
                    "INSERT INTO ros_conversations \
                        (id, session_key, agent, channel, settings, active, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, 'cli', '{}', 1, ?4, ?4)",
                    params![id, session, agent, common::RECORDED_START],
                )
                .map_err(common::seed)?;
            }
            Ok(())
        })
        .await?;
    Ok(())
}

async fn seed_merge_tags(store: &SqliteStore) -> TestResult {
    let rows = [
        (
            "20000000-0000-4000-8000-000000000001",
            "20000000-0000-4000-8000-000000000011",
            "acme",
        ),
        (
            "20000000-0000-4000-8000-000000000002",
            "20000000-0000-4000-8000-000000000011",
            "acme-app",
        ),
        (
            "20000000-0000-4000-8000-000000000003",
            "20000000-0000-4000-8000-000000000012",
            "acme-app",
        ),
    ];
    store
        .with_conn(move |conn| {
            for (id, entity, value) in rows {
                conn.execute(
                    "INSERT INTO ros_tags \
                        (id, entity_type, entity_id, key, value, display, source, state, \
                         created_at, updated_at) \
                     VALUES (?1, 'conversation', ?2, 'project', ?3, '', 'user', 'accepted', ?4, ?4)",
                    params![id, entity, value, common::RECORDED_START],
                )
                .map_err(common::seed)?;
            }
            Ok(())
        })
        .await?;
    Ok(())
}

async fn add_ok(
    store: &SqliteStore,
    step: &Step,
    ids: &mut Ids,
    add: TagAdd,
) -> TestResult<String> {
    let store = pinned(store, step)?;
    let tag = store.add_tag(add).await?;
    expect_json(
        recorded(step)?,
        &serde_json::to_value(&tag)?,
        &step.label,
        ids,
    )?;
    Ok(tag.id)
}

async fn add_err(store: &SqliteStore, step: &Step, add: TagAdd) -> TestResult {
    let store = pinned(store, step)?;
    match store.add_tag(add).await {
        Err(error) => expect_text(
            &step.error.as_ref().ok_or("add error")?.message,
            &error.to_string(),
        ),
        Ok(_) => Err(format!("{} succeeded", step.label).into()),
    }
}

async fn expect_list(
    store: &SqliteStore,
    step: &Step,
    ids: &mut Ids,
    key: Option<&str>,
    states: Vec<TagState>,
) -> TestResult {
    let tags = store
        .list_tags(TagListQuery {
            entity_type: None,
            entity_id: None,
            key: key.map(str::to_owned),
            value: None,
            states,
            limit: page(),
        })
        .await?;
    expect_json(
        recorded(step)?,
        &serde_json::to_value(&tags)?,
        &step.label,
        ids,
    )
}

async fn expect_counts(
    store: &SqliteStore,
    step: &Step,
    ids: &mut Ids,
    key: Option<&str>,
) -> TestResult {
    let counts = store
        .tag_counts(TagCountsQuery {
            key: key.map(str::to_owned),
            limit: page(),
        })
        .await?;
    expect_json(
        recorded(step)?,
        &serde_json::to_value(&counts)?,
        &step.label,
        ids,
    )
}

async fn upsert_ok(
    store: &SqliteStore,
    step: &Step,
    ids: &mut Ids,
    upsert: TagTaxonomyUpsert,
) -> TestResult {
    let store = pinned(store, step)?;
    let entry = store.upsert_tag_taxonomy(upsert).await?;
    expect_json(
        recorded(step)?,
        &serde_json::to_value(&entry)?,
        &step.label,
        ids,
    )
}

async fn upsert_err(store: &SqliteStore, step: &Step, upsert: TagTaxonomyUpsert) -> TestResult {
    let store = pinned(store, step)?;
    match store.upsert_tag_taxonomy(upsert).await {
        Err(error) => expect_text(
            &step.error.as_ref().ok_or("upsert error")?.message,
            &error.to_string(),
        ),
        Ok(_) => Err(format!("{} succeeded", step.label).into()),
    }
}

async fn expect_taxonomy(
    store: &SqliteStore,
    step: &Step,
    ids: &mut Ids,
    key: Option<&str>,
    by_value: bool,
) -> TestResult {
    let entries = store.list_tag_taxonomy(taxonomy_query(key)).await?;
    let actual = serde_json::to_value(&entries)?;
    let recorded = recorded(step)?;
    if by_value {
        expect_by_identity(recorded, &actual, &step.label, ids)
    } else {
        expect_json(recorded, &actual, &step.label, ids)
    }
}

async fn expect_wrote(
    store: &SqliteStore,
    step: &Step,
    ids: &mut Ids,
    proposals: &[TagVocabularyProposal],
) -> TestResult {
    let at = instant(&step.at)?;
    let wrote = store
        .store_tag_suggestions(TagSuggestionWrite {
            proposed_by: "tagger-v1".to_owned(),
            at,
            rows: Vec::new(),
            vocabulary: proposals.to_vec(),
        })
        .await?;
    expect_json(
        recorded(step)?,
        &Json::from(wrote.vocabulary),
        &step.label,
        ids,
    )
}

async fn expect_merge(
    store: &SqliteStore,
    step: &Step,
    ids: &mut Ids,
    from: &str,
    into: &str,
) -> TestResult {
    let store = pinned(store, step)?;
    let result = store
        .merge_tag_taxonomy(TagTaxonomyMerge {
            key: "project".to_owned(),
            from: from.to_owned(),
            into: into.to_owned(),
        })
        .await?;
    expect_json(
        recorded(step)?,
        &serde_json::to_value(&result)?,
        &step.label,
        ids,
    )
}

async fn merge_err(store: &SqliteStore, step: &Step, from: &str, into: &str) -> TestResult {
    let store = pinned(store, step)?;
    let outcome = store
        .merge_tag_taxonomy(TagTaxonomyMerge {
            key: "project".to_owned(),
            from: from.to_owned(),
            into: into.to_owned(),
        })
        .await;
    match outcome {
        Err(error) => expect_text(
            &step.error.as_ref().ok_or("merge error")?.message,
            &error.to_string(),
        ),
        Ok(_) => Err(format!("{} succeeded", step.label).into()),
    }
}

async fn compare_saved(
    store: &SqliteStore,
    scenario: &Scenario,
    ids: &mut Ids,
    name: &str,
) -> TestResult {
    common::compare_tables(store, &scenario.tables, ids, name).await
}

fn session_add(
    session: &str,
    literal: &str,
    display: Option<&str>,
    reason: Option<&str>,
    agent: Option<&str>,
    decided_by: Option<&str>,
) -> TestResult<TagAdd> {
    Ok(TagAdd {
        entity_type: TagEntityType::Conversation,
        entity: TagEntity::SessionKey(session.to_owned()),
        agent: agent.map(str::to_owned),
        tag: TagLiteral::parse_add(literal)?,
        display: display.map(str::to_owned),
        reason: reason.map(str::to_owned),
        decided_by: decided_by.map(str::to_owned),
    })
}

fn vocab(
    key: &str,
    value: &str,
    display: Option<&str>,
    parent: Option<&str>,
    aliases: &[&str],
    reason: Option<&str>,
) -> TagTaxonomyUpsert {
    TagTaxonomyUpsert {
        key: key.to_owned(),
        value: value.to_owned(),
        display: display.map(str::to_owned),
        parent_value: parent.map(str::to_owned),
        aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
        reason: reason.map(str::to_owned),
    }
}

fn taxonomy_query(key: Option<&str>) -> TagTaxonomyQuery {
    TagTaxonomyQuery {
        key: key.map(str::to_owned),
        state: None,
        limit: page(),
    }
}

fn page() -> Limit {
    Limit::clamped(Some(200), 50, 1..=200)
}

fn tag_id(id: &str) -> TestResult<TagId> {
    Ok(TagId::from_uuid(Uuid::parse_str(id)?))
}

fn expect_id_count(step: &Step, changed: u64, actual_id: &str, ids: &mut Ids) -> TestResult {
    let recorded = recorded(step)?
        .as_array()
        .ok_or_else(|| format!("{} has no id list", step.label))?;
    let len = u64::try_from(recorded.len())?;
    if changed != len {
        return Err(format!("{changed} tags changed; the recording lists {len}").into());
    }
    for recorded_id in recorded {
        let recorded_id = recorded_id
            .as_str()
            .ok_or_else(|| format!("{} id", step.label))?;
        ids.same(
            &Json::String(recorded_id.to_owned()),
            &Json::String(actual_id.to_owned()),
            &step.label,
        )?;
    }
    Ok(())
}

fn expect_by_identity(recorded: &Json, actual: &Json, at: &str, ids: &mut Ids) -> TestResult {
    let recorded_rows = recorded.as_array().ok_or("recorded rows")?;
    let actual_rows = actual.as_array().ok_or("actual rows")?;
    if recorded_rows.len() != actual_rows.len() {
        return Err(format!(
            "{at}: {} recorded rows vs {} actual",
            recorded_rows.len(),
            actual_rows.len()
        )
        .into());
    }
    for recorded_row in recorded_rows {
        let key = text_field(recorded_row, "key")?;
        let value = text_field(recorded_row, "value")?;
        let found = actual_rows.iter().find(|row| {
            matches!(
                (text_field(row, "key"), text_field(row, "value")),
                (Ok(found_key), Ok(found_value)) if found_key == key && found_value == value
            )
        });
        let Some(found) = found else {
            return Err(format!("{at}: missing {key}:{value}").into());
        };
        expect_json(recorded_row, found, &format!("{at} {key}:{value}"), ids)?;
    }
    Ok(())
}

fn project_taxonomy(row: &Json) -> TestResult<Json> {
    let row: TaxonomyRow = serde_json::from_value(row.clone())?;
    let entry = TagTaxonomyEntry {
        key: row.key,
        value: row.value,
        display: row.display,
        parent_value: row.parent_value,
        aliases: serde_json::from_str(&row.aliases)?,
        state: row.state,
        source: row.source,
        reason: row.reason,
        decided_at: row.decided_at.as_deref().map(parse_time).transpose()?,
        created_at: parse_time(&row.created_at)?,
        updated_at: parse_time(&row.updated_at)?,
    };
    Ok(serde_json::to_value(entry)?)
}

#[derive(Deserialize)]
struct TaxonomyRow {
    key: String,
    value: String,
    display: String,
    parent_value: Option<String>,
    aliases: String,
    state: TagState,
    source: String,
    reason: String,
    decided_at: Option<String>,
    created_at: String,
    updated_at: String,
}

fn parse_time(text: &str) -> TestResult<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(text)?.with_timezone(&Utc))
}

fn decode_map(value: &Json) -> TestResult<Json> {
    let pairs = value.get("$map").and_then(Json::as_array).ok_or("$map")?;
    let mut map = BTreeMap::new();
    for pair in pairs {
        let items = pair.as_array().ok_or("map pair")?;
        let key = items.first().and_then(Json::as_str).ok_or("map key")?;
        let tags = items.get(1).ok_or("map tags")?.clone();
        map.insert(key.to_owned(), tags);
    }
    Ok(serde_json::to_value(map)?)
}

fn named<'a>(file: &'a ScenarioFile, name: &str) -> TestResult<&'a Scenario> {
    file.scenarios
        .iter()
        .find(|scenario| scenario.name == name)
        .ok_or_else(|| format!("missing scenario {name}").into())
}

fn step<'a>(scenario: &'a Scenario, label: &str) -> TestResult<&'a Step> {
    scenario
        .steps
        .iter()
        .find(|step| step.label == label)
        .ok_or_else(|| format!("missing step {label}").into())
}

fn recorded(step: &Step) -> TestResult<&Json> {
    step.result
        .as_ref()
        .ok_or_else(|| format!("{} has no result", step.label).into())
}

fn pinned(store: &SqliteStore, step: &Step) -> TestResult<SqliteStore> {
    let at = instant(&step.at)?;
    Ok(store.clone().with_clock(move || at))
}

fn vocabulary_proposal(key: &str, value: &str) -> TagVocabularyProposal {
    TagVocabularyProposal {
        key: key.to_owned(),
        value: value.to_owned(),
        display: String::new(),
        reason: "proposed by tagger-v1".to_owned(),
    }
}

fn expect_text(recorded: &str, actual: &str) -> TestResult {
    if recorded == actual {
        Ok(())
    } else {
        Err(format!("recorded {recorded:?} vs actual {actual:?}").into())
    }
}

fn text_field<'a>(row: &'a Json, key: &str) -> TestResult<&'a str> {
    row.get(key)
        .and_then(Json::as_str)
        .ok_or_else(|| format!("missing {key}").into())
}
