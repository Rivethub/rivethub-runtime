use crate::common;

use std::collections::HashMap;
use std::str::FromStr as _;

use common::Ids;
use common::TestResult;
use common::fresh_store;
use common::instant;
use common::recording;
use kernel::memory::MemoryStore;
use kernel::memory::MemoryStoreError;
use kernel::memory::TagDecision;
use kernel::memory::TagEntityType;
use kernel::memory::TagId;
use kernel::memory::TagListQuery;
use kernel::memory::TagState;
use kernel::memory::TagSuggestKind;
use kernel::memory::TagSuggestionRow;
use kernel::memory::TagSuggestionStored;
use kernel::memory::TagSuggestionWrite;
use kernel::memory::TagVocabularyProposal;
use memory_sqlite::SqliteStore;
use rusqlite::params;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use testkit::Json;
use uuid::Uuid;

const NATIVE: &str = "native tagger request shape";
const REMOVALS: &str = "protected removals allowed";
const CHAT: &str = "tagger suggestions (chat), acceptance, removals";

#[tokio::test]
async fn native_suggestion_rows_match_the_recording() -> TestResult {
    let file: SuggestFile = recording("tags.json")?;
    let scenario = named(&file, NATIVE)?;
    let pending = pending_head(scenario)?;
    let body = &scenario.native_posts.first().ok_or("native post")?.body;
    let tags: Vec<RecordedTag> = table_rows(scenario, "ros_tags")?;
    let vocab: Vec<RecordedVocab> = table_rows(scenario, "ros_tag_taxonomy")?;
    let summary_id = Uuid::parse_str(&pending.entity_id)?;
    let conversation_id = Uuid::parse_str(&pending.conversation_id)?;
    let fresh = fresh_store()?;
    seed_native_rows(
        &fresh.store,
        summary_id,
        conversation_id,
        &pending,
        body,
        &vocab,
    )
    .await?;
    assert_open_context(&fresh.store, summary_id, conversation_id, &pending, body).await?;

    let write = suggestion_write(&tags, &vocab)?;
    let stored = fresh.store.store_tag_suggestions(write.clone()).await?;
    assert_eq!(stored, stored_counts(&tags, &vocab)?);
    common::compare_tables(&fresh.store, &scenario.tables, &mut Ids::default(), NATIVE).await?;

    let again = fresh.store.store_tag_suggestions(write).await?;
    assert_eq!(again, empty_stored());
    common::compare_tables(&fresh.store, &scenario.tables, &mut Ids::default(), NATIVE).await?;
    assert_loaded_context(&fresh.store, summary_id, &tags, &vocab).await?;
    Ok(())
}

#[tokio::test]
async fn repeated_suggestions_leave_an_accepted_tag() -> TestResult {
    let file: SuggestFile = recording("tags.json")?;
    let scenario = named(&file, NATIVE)?;
    let pending = pending_head(scenario)?;
    let body = &scenario.native_posts.first().ok_or("native post")?.body;
    let tags: Vec<RecordedTag> = table_rows(scenario, "ros_tags")?;
    let vocab: Vec<RecordedVocab> = table_rows(scenario, "ros_tag_taxonomy")?;
    let summary_id = Uuid::parse_str(&pending.entity_id)?;
    let conversation_id = Uuid::parse_str(&pending.conversation_id)?;
    let fresh = fresh_store()?;
    let at = instant(&pending.created_at)?;
    let store = fresh.store.clone().with_clock(move || at);
    seed_conversation(
        &store,
        conversation_id,
        &pending.session_key,
        &pending.agent,
        pending.title.as_deref(),
        &pending.created_at,
    )
    .await?;
    seed_summary(
        &store,
        summary_id,
        Some(conversation_id),
        &body.text,
        &pending.created_at,
    )
    .await?;
    seed_prior_vocabulary(&store, &vocab).await?;
    let write = suggestion_write(&tags, &vocab)?;
    store.store_tag_suggestions(write.clone()).await?;
    let kept = tags
        .iter()
        .find(|tag| tag.entity_type == "summary" && tag.key == "project")
        .ok_or("summary project suggestion")?;
    let id = tag_id(&store, summary_id, &kept.key, &kept.value).await?;
    let changed = store
        .decide_tags(
            vec![TagId::from_uuid(id)],
            TagDecision::Accept,
            Some("owner".to_owned()),
        )
        .await?;
    assert_eq!(changed, 1);
    let before = listed(&store, summary_id, &kept.key, &kept.value).await?;
    let stored = store.store_tag_suggestions(write).await?;
    assert_eq!(stored.tags, 0);
    let after = listed(&store, summary_id, &kept.key, &kept.value).await?;
    assert_eq!(after.state, TagState::Accepted);
    assert_eq!(after.source, before.source);
    assert_eq!(after.display, before.display);
    assert_eq!(after.confidence, before.confidence);
    assert_eq!(after.proposed_by, before.proposed_by);
    assert_eq!(after.reason, before.reason);
    Ok(())
}

#[tokio::test]
async fn removal_rows_match_the_recording() -> TestResult {
    let file: SuggestFile = recording("tags.json")?;
    let scenario = named(&file, REMOVALS)?;
    let tags: Vec<RecordedTag> = table_rows(scenario, "ros_tags")?;
    let recorded = tags.first().ok_or("removal row")?;
    let fresh = fresh_store()?;
    seed_tag(&fresh.store, recorded, true).await?;
    let entity_id = Uuid::parse_str(&recorded.entity_id)?;
    let stored = fresh
        .store
        .store_tag_suggestions(TagSuggestionWrite {
            proposed_by: recorded.proposed_by.clone(),
            at: instant(&recorded.updated_at)?,
            rows: vec![TagSuggestionRow {
                kind: TagSuggestKind::Remove,
                entity_type: TagEntityType::from_str(&recorded.entity_type)?,
                entity_id,
                key: recorded.key.clone(),
                value: recorded.value.clone(),
                display: recorded.display.clone(),
                confidence: recorded.confidence,
                reason: Some(recorded.removal_reason.clone()),
            }],
            vocabulary: Vec::new(),
        })
        .await?;
    assert_eq!(
        stored,
        TagSuggestionStored {
            tags: 0,
            removals: 1,
            vocabulary: 0,
        }
    );
    common::compare_tables(
        &fresh.store,
        &scenario.tables,
        &mut Ids::default(),
        REMOVALS,
    )
    .await?;
    let again = fresh
        .store
        .store_tag_suggestions(TagSuggestionWrite {
            proposed_by: recorded.proposed_by.clone(),
            at: instant(&recorded.updated_at)?,
            rows: vec![TagSuggestionRow {
                kind: TagSuggestKind::Remove,
                entity_type: TagEntityType::from_str(&recorded.entity_type)?,
                entity_id,
                key: recorded.key.clone(),
                value: recorded.value.clone(),
                display: recorded.display.clone(),
                confidence: None,
                reason: Some("twice".to_owned()),
            }],
            vocabulary: Vec::new(),
        })
        .await?;
    assert_eq!(again.removals, 0);
    common::compare_tables(
        &fresh.store,
        &scenario.tables,
        &mut Ids::default(),
        REMOVALS,
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn removals_skip_tags_that_are_not_an_open_acceptance() -> TestResult {
    let fresh = fresh_store()?;
    let at = instant(common::RECORDED_START)?;
    let conversation = Uuid::parse_str("00000000-0000-4000-8000-000000000002")?;
    seed_plain(&fresh.store, conversation, "manual", "accepted", None, "").await?;
    seed_plain(
        &fresh.store,
        conversation,
        "kept",
        "accepted",
        Some("rejected"),
        "nope",
    )
    .await?;
    seed_plain(
        &fresh.store,
        conversation,
        "already",
        "accepted",
        Some("suggested"),
        "again",
    )
    .await?;
    seed_plain(&fresh.store, conversation, "open", "suggested", None, "").await?;
    let stored = fresh
        .store
        .store_tag_suggestions(TagSuggestionWrite {
            proposed_by: "tagger-v1".to_owned(),
            at,
            rows: vec![
                remove(conversation, "manual", "allowed now"),
                remove(conversation, "kept", "nope"),
                remove(conversation, "already", "again"),
                remove(conversation, "open", "not accepted"),
                remove(conversation, "missing", "absent"),
            ],
            vocabulary: Vec::new(),
        })
        .await?;
    assert_eq!(stored.removals, 1);
    assert_eq!(stored.tags, 0);
    let rows = common::table(&fresh.store, "ros_tags").await?;
    assert_eq!(
        removal_of(&rows, "manual")?,
        (Some("suggested".to_owned()), "allowed now".to_owned())
    );
    assert_eq!(
        removal_of(&rows, "kept")?,
        (Some("rejected".to_owned()), "nope".to_owned())
    );
    assert_eq!(
        removal_of(&rows, "already")?,
        (Some("suggested".to_owned()), "again".to_owned())
    );
    assert_eq!(removal_of(&rows, "open")?, (None, String::new()));
    Ok(())
}

#[tokio::test]
async fn chat_release_notes_do_not_reset_the_first_suggestions() -> TestResult {
    let file: SuggestFile = recording("tags.json")?;
    let native = named(&file, NATIVE)?;
    let chat = named(&file, CHAT)?;
    let pending = pending_head(native)?;
    let body = &native.native_posts.first().ok_or("native post")?.body;
    let tags: Vec<RecordedTag> = table_rows(native, "ros_tags")?;
    let vocab: Vec<RecordedVocab> = table_rows(native, "ros_tag_taxonomy")?;
    let summary_id = Uuid::parse_str(&pending.entity_id)?;
    let conversation_id = Uuid::parse_str(&pending.conversation_id)?;
    let fresh = fresh_store()?;
    seed_conversation(
        &fresh.store,
        conversation_id,
        &pending.session_key,
        &pending.agent,
        pending.title.as_deref(),
        &pending.created_at,
    )
    .await?;
    seed_summary(
        &fresh.store,
        summary_id,
        Some(conversation_id),
        &body.text,
        &pending.created_at,
    )
    .await?;
    seed_prior_vocabulary(&fresh.store, &vocab).await?;
    fresh
        .store
        .store_tag_suggestions(suggestion_write(&tags, &vocab)?)
        .await?;
    let before = common::table(&fresh.store, "ros_tags").await?;
    let chat_tags: Vec<RecordedTag> = table_rows(chat, "ros_tags")?;
    let chat_vocab: Vec<RecordedVocab> = table_rows(chat, "ros_tag_taxonomy")?;
    let release: Vec<RecordedTag> = chat_tags
        .into_iter()
        .filter(|tag| tag.value == "release-notes")
        .collect();
    for row in &release {
        if row.entity_type == "summary" {
            let id = Uuid::parse_str(&row.entity_id)?;
            if id != summary_id {
                seed_summary(
                    &fresh.store,
                    id,
                    Some(conversation_id),
                    "Release note summary.",
                    &pending.created_at,
                )
                .await?;
            }
        }
    }
    let release_vocab: Vec<RecordedVocab> = chat_vocab
        .into_iter()
        .filter(|row| row.value == "release-notes")
        .collect();
    assert_eq!(release.len(), 2);
    let stored = fresh
        .store
        .store_tag_suggestions(suggestion_write(&release, &release_vocab)?)
        .await?;
    assert_eq!(
        stored,
        TagSuggestionStored {
            tags: 2,
            removals: 0,
            vocabulary: 1,
        }
    );
    let after = common::table(&fresh.store, "ros_tags").await?;
    assert_eq!(after.len(), before.len() + release.len());
    for earlier in &before {
        assert!(after.iter().any(|row| row == earlier));
    }
    Ok(())
}

#[tokio::test]
async fn the_vocabulary_adds_accepted_tags_in_use() -> TestResult {
    let fresh = fresh_store()?;
    let summary = Uuid::parse_str("00000000-0000-4000-8000-000000000041")?;
    let conversation = Uuid::parse_str("00000000-0000-4000-8000-000000000042")?;
    let at = "2026-01-01T00:00:00.000Z";
    seed_conversation(&fresh.store, conversation, "cli:vocab", "rivet", None, at).await?;
    seed_summary(&fresh.store, summary, Some(conversation), "body", at).await?;
    common::execute(
        &fresh.store,
        "INSERT INTO ros_tag_taxonomy (key, value, state, created_at, updated_at) VALUES \
           ('project', 'scratch', 'rejected', '2026-01-01T00:00:01.000Z', '2026-01-01T00:00:01.000Z'), \
           ('topic', 'storage', 'accepted', '2026-01-01T00:00:02.000Z', '2026-01-01T00:00:02.000Z'); \
         INSERT INTO ros_tags (id, entity_type, entity_id, key, value, display, source, state, \
                               created_at, updated_at) VALUES \
           ('t1', 'conversation', 'other', 'project', 'Widget Hub', 'Widget Hub', 'user', 'accepted', \
            '2026-01-01T00:00:03.000Z', '2026-01-01T00:00:03.000Z'), \
           ('t2', 'conversation', 'other', 'project', 'scratch', '', 'user', 'accepted', \
            '2026-01-01T00:00:03.000Z', '2026-01-01T00:00:03.000Z'), \
           ('t3', 'conversation', 'other', 'topic', 'storage', '', 'user', 'accepted', \
            '2026-01-01T00:00:03.000Z', '2026-01-01T00:00:03.000Z'), \
           ('t4', 'conversation', 'other', 'topic', 'maybe', '', 'model', 'suggested', \
            '2026-01-01T00:00:03.000Z', '2026-01-01T00:00:03.000Z');",
    )
    .await?;
    let loaded = fresh
        .store
        .tag_job_context(summary)
        .await?
        .ok_or("context")?;
    let seen: Vec<(&str, &str, TagState)> = loaded
        .vocabulary
        .iter()
        .map(|entry| (entry.key.as_str(), entry.value.as_str(), entry.state))
        .collect();
    assert_eq!(
        seen,
        vec![
            ("project", "scratch", TagState::Rejected),
            ("topic", "storage", TagState::Accepted),
            ("project", "Widget Hub", TagState::Accepted),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn missing_summaries_and_missing_tables_fail_closed() -> TestResult {
    let fresh = fresh_store()?;
    let summary = assert_missing_summary_heads(&fresh.store).await?;
    assert_missing_tag_tables(&fresh.store, summary).await?;
    Ok(())
}

#[tokio::test]
async fn a_check_failure_rolls_the_suggestion_write_back() -> TestResult {
    let fresh = fresh_store()?;
    let summary = Uuid::parse_str("00000000-0000-4000-8000-000000000040")?;
    seed_summary(
        &fresh.store,
        summary,
        None,
        "Suggestion source.",
        common::RECORDED_START,
    )
    .await?;
    let stored = fresh
        .store
        .store_tag_suggestions(TagSuggestionWrite {
            proposed_by: "tagger-v1".to_owned(),
            at: instant(common::RECORDED_START)?,
            rows: vec![
                add(
                    TagEntityType::Summary,
                    summary,
                    "topic",
                    "notes",
                    "Notes",
                    Some(0.5),
                ),
                add(
                    TagEntityType::Summary,
                    summary,
                    "topic",
                    "wide",
                    "Wide",
                    Some(1.5),
                ),
            ],
            vocabulary: vec![TagVocabularyProposal {
                key: "topic".to_owned(),
                value: "notes".to_owned(),
                display: "Notes".to_owned(),
                reason: "proposed by tagger-v1".to_owned(),
            }],
        })
        .await;
    assert!(matches!(
        stored,
        Err(MemoryStoreError::InvalidRequest { .. })
    ));
    assert_eq!(
        common::table(&fresh.store, "ros_tags").await?,
        Vec::<Json>::new()
    );
    assert_eq!(
        common::table(&fresh.store, "ros_tag_taxonomy").await?,
        Vec::<Json>::new()
    );
    Ok(())
}

#[derive(Debug, Deserialize)]
struct SuggestFile {
    scenarios: Vec<SuggestScenario>,
}

#[derive(Debug, Deserialize)]
struct SuggestScenario {
    name: String,
    #[serde(default)]
    steps: Vec<SuggestStep>,
    #[serde(default)]
    native_posts: Vec<NativePost>,
    #[serde(default)]
    tables: HashMap<String, Vec<Json>>,
}

#[derive(Debug, Deserialize)]
struct SuggestStep {
    label: String,
    result: Option<Json>,
}

#[derive(Debug, Deserialize)]
struct NativePost {
    body: NativeBody,
}

#[derive(Debug, Deserialize)]
struct NativeBody {
    text: String,
    title: String,
    agent: String,
    vocabulary: Vec<String>,
    current: Vec<Json>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingHead {
    entity_id: String,
    session_key: String,
    title: Option<String>,
    agent: String,
    conversation_id: String,
    created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RecordedTag {
    id: String,
    entity_type: String,
    entity_id: String,
    key: String,
    value: String,
    display: String,
    source: String,
    state: String,
    confidence: Option<f64>,
    proposed_by: String,
    reason: String,
    decided_by: Option<String>,
    decided_at: Option<String>,
    created_at: String,
    updated_at: String,
    removal_state: Option<String>,
    removal_reason: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RecordedVocab {
    key: String,
    value: String,
    display: String,
    aliases: String,
    state: String,
    source: String,
    reason: String,
    decided_at: Option<String>,
    created_at: String,
    updated_at: String,
}

fn named<'a>(file: &'a SuggestFile, name: &str) -> TestResult<&'a SuggestScenario> {
    file.scenarios
        .iter()
        .find(|scenario| scenario.name == name)
        .ok_or_else(|| format!("missing scenario {name}").into())
}

fn pending_head(scenario: &SuggestScenario) -> TestResult<PendingHead> {
    let step = scenario
        .steps
        .iter()
        .find(|step| step.label == "pending")
        .ok_or("pending step")?;
    let result = step.result.as_ref().ok_or("pending result")?;
    let rows: Vec<PendingHead> = serde_json::from_value(result.clone())?;
    rows.into_iter()
        .next()
        .ok_or_else(|| "empty pending".into())
}

fn table_rows<T: DeserializeOwned>(scenario: &SuggestScenario, name: &str) -> TestResult<Vec<T>> {
    let rows = scenario
        .tables
        .get(name)
        .ok_or_else(|| format!("missing table {name}"))?;
    rows.iter()
        .map(|row| Ok(serde_json::from_value(row.clone())?))
        .collect()
}

fn suggested_vocab(rows: &[RecordedVocab]) -> TestResult<Vec<&RecordedVocab>> {
    let mut seen_suggested = false;
    let mut suggested = Vec::new();
    for row in rows {
        if row.state == "suggested" {
            seen_suggested = true;
            suggested.push(row);
        } else if seen_suggested {
            return Err("suggested vocabulary is mixed into earlier rows".into());
        }
    }
    Ok(suggested)
}

fn vocab_in_read_order(rows: &[RecordedVocab]) -> Vec<RecordedVocab> {
    let mut ordered = rows.to_vec();
    ordered.sort_by(|left, right| {
        (&left.created_at, &left.key, &left.value).cmp(&(
            &right.created_at,
            &right.key,
            &right.value,
        ))
    });
    ordered
}

fn suggestion_write(
    tags: &[RecordedTag],
    vocab: &[RecordedVocab],
) -> TestResult<TagSuggestionWrite> {
    let first = tags.first().ok_or("suggestion rows")?;
    let at = instant(&first.created_at)?;
    if tags
        .iter()
        .any(|tag| tag.created_at != first.created_at || tag.proposed_by != first.proposed_by)
    {
        return Err("suggestion rows do not share one clock and proposer".into());
    }
    let proposals = suggested_vocab(vocab)?;
    if proposals
        .iter()
        .any(|row| row.created_at != first.created_at)
    {
        return Err("vocabulary suggestions do not share the tag clock".into());
    }
    let mut rows = Vec::new();
    for tag in tags {
        rows.push(TagSuggestionRow {
            kind: TagSuggestKind::Add,
            entity_type: TagEntityType::from_str(&tag.entity_type)?,
            entity_id: Uuid::parse_str(&tag.entity_id)?,
            key: tag.key.clone(),
            value: tag.value.clone(),
            display: tag.display.clone(),
            confidence: tag.confidence,
            reason: Some(tag.reason.clone()),
        });
    }
    let mut vocabulary = Vec::new();
    for row in proposals {
        vocabulary.push(TagVocabularyProposal {
            key: row.key.clone(),
            value: row.value.clone(),
            display: row.display.clone(),
            reason: row.reason.clone(),
        });
    }
    Ok(TagSuggestionWrite {
        proposed_by: first.proposed_by.clone(),
        at,
        rows,
        vocabulary,
    })
}

fn empty_stored() -> TagSuggestionStored {
    TagSuggestionStored {
        tags: 0,
        removals: 0,
        vocabulary: 0,
    }
}

fn stored_counts(tags: &[RecordedTag], vocab: &[RecordedVocab]) -> TestResult<TagSuggestionStored> {
    Ok(TagSuggestionStored {
        tags: u64::try_from(tags.len())?,
        removals: 0,
        vocabulary: u64::try_from(suggested_vocab(vocab)?.len())?,
    })
}

async fn seed_native_rows(
    store: &SqliteStore,
    summary_id: Uuid,
    conversation_id: Uuid,
    pending: &PendingHead,
    body: &NativeBody,
    vocab: &[RecordedVocab],
) -> TestResult {
    seed_conversation(
        store,
        conversation_id,
        &pending.session_key,
        &pending.agent,
        pending.title.as_deref(),
        &pending.created_at,
    )
    .await?;
    seed_summary(
        store,
        summary_id,
        Some(conversation_id),
        &body.text,
        &pending.created_at,
    )
    .await?;
    seed_prior_vocabulary(store, vocab).await
}

async fn assert_open_context(
    store: &SqliteStore,
    summary_id: Uuid,
    conversation_id: Uuid,
    pending: &PendingHead,
    body: &NativeBody,
) -> TestResult {
    let context = store
        .tag_job_context(summary_id)
        .await?
        .ok_or("summary context")?;
    assert_eq!(context.summary_id, summary_id);
    assert_eq!(context.conversation_id, conversation_id);
    assert_eq!(context.session_key, pending.session_key);
    assert_eq!(context.title, pending.title);
    assert_eq!(context.title.as_deref(), Some(body.title.as_str()));
    assert_eq!(context.agent, pending.agent);
    assert_eq!(context.agent, body.agent);
    assert_eq!(context.summary, body.text);
    assert_eq!(context.tags, Vec::new());
    assert_eq!(body.current, Vec::<Json>::new());
    let rendered: Vec<String> = context
        .vocabulary
        .iter()
        .map(|entry| format!("{}:{}", entry.key, entry.value))
        .collect();
    assert_eq!(rendered, body.vocabulary);
    assert!(
        context
            .vocabulary
            .iter()
            .all(|entry| entry.aliases.is_empty())
    );
    Ok(())
}

async fn assert_loaded_context(
    store: &SqliteStore,
    summary_id: Uuid,
    tags: &[RecordedTag],
    vocab: &[RecordedVocab],
) -> TestResult {
    let loaded = store
        .tag_job_context(summary_id)
        .await?
        .ok_or("context after suggestions")?;
    assert_eq!(loaded.tags.len(), tags.len());
    for recorded in tags {
        let entity = TagEntityType::from_str(&recorded.entity_type)?;
        let found = loaded
            .tags
            .iter()
            .find(|tag| {
                tag.entity_type == entity && tag.key == recorded.key && tag.value == recorded.value
            })
            .ok_or("recorded suggestion missing from the context")?;
        assert_eq!(found.display, recorded.display);
        assert_eq!(found.source, recorded.source);
        assert_eq!(found.state, TagState::from_str(&recorded.state)?);
    }
    assert_eq!(loaded.vocabulary.len(), vocab.len());
    for (actual, recorded) in loaded.vocabulary.iter().zip(vocab_in_read_order(vocab)) {
        assert_eq!(actual.key, recorded.key);
        assert_eq!(actual.value, recorded.value);
        assert_eq!(actual.display, recorded.display);
        assert_eq!(actual.state, TagState::from_str(&recorded.state)?);
        let aliases: Vec<String> = serde_json::from_str(&recorded.aliases)?;
        assert_eq!(actual.aliases, aliases);
    }
    Ok(())
}

async fn assert_missing_summary_heads(store: &SqliteStore) -> TestResult<Uuid> {
    let absent = Uuid::parse_str("00000000-0000-4000-8000-000000000030")?;
    assert!(store.tag_job_context(absent).await?.is_none());
    let bare = Uuid::parse_str("00000000-0000-4000-8000-000000000031")?;
    seed_summary(store, bare, None, "no conversation", common::RECORDED_START).await?;
    let orphan = store.tag_job_context(bare).await;
    assert_eq!(
        message_of(orphan),
        format!("summary {bare} has no conversation")
    );
    let conversation = Uuid::parse_str("00000000-0000-4000-8000-000000000032")?;
    let summary = Uuid::parse_str("00000000-0000-4000-8000-000000000033")?;
    seed_conversation(
        store,
        conversation,
        "cli:missing",
        "rivet",
        None,
        common::RECORDED_START,
    )
    .await?;
    seed_summary(
        store,
        summary,
        Some(conversation),
        "body",
        common::RECORDED_START,
    )
    .await?;
    Ok(summary)
}

async fn assert_missing_tag_tables(store: &SqliteStore, summary: Uuid) -> TestResult {
    common::execute(store, "DROP TABLE ros_tags; DROP TABLE ros_tag_taxonomy;").await?;
    let context = store.tag_job_context(summary).await;
    assert!(matches!(context, Err(MemoryStoreError::TagTablesMissing)));
    let stored = store
        .store_tag_suggestions(TagSuggestionWrite {
            proposed_by: "tagger-v1".to_owned(),
            at: instant(common::RECORDED_START)?,
            rows: vec![add(
                TagEntityType::Summary,
                summary,
                "topic",
                "notes",
                "",
                None,
            )],
            vocabulary: Vec::new(),
        })
        .await;
    assert!(matches!(stored, Err(MemoryStoreError::TagTablesMissing)));
    let empty = store
        .store_tag_suggestions(TagSuggestionWrite {
            proposed_by: "tagger-v1".to_owned(),
            at: instant(common::RECORDED_START)?,
            rows: Vec::new(),
            vocabulary: Vec::new(),
        })
        .await?;
    assert_eq!(empty, empty_stored());
    Ok(())
}

async fn seed_conversation(
    store: &SqliteStore,
    id: Uuid,
    session: &str,
    agent: &str,
    title: Option<&str>,
    at: &str,
) -> TestResult {
    let id = id.to_string();
    let session = session.to_owned();
    let agent = agent.to_owned();
    let title = title.map(str::to_owned);
    let at = at.to_owned();
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO ros_conversations \
                    (id, session_key, agent, channel, title, settings, active, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, 'cli', ?4, '{}', 1, ?5, ?5)",
                params![id, session, agent, title, at],
            )
            .map_err(common::seed)?;
            Ok(())
        })
        .await?;
    Ok(())
}

async fn seed_summary(
    store: &SqliteStore,
    id: Uuid,
    conversation: Option<Uuid>,
    content: &str,
    at: &str,
) -> TestResult {
    let id = id.to_string();
    let conversation = conversation.map(|value| value.to_string());
    let content = content.to_owned();
    let at = at.to_owned();
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO ros_summaries (id, conversation_id, content, kind, created_at) \
                 VALUES (?1, ?2, ?3, 'leaf', ?4)",
                params![id, conversation, content, at],
            )
            .map_err(common::seed)?;
            Ok(())
        })
        .await?;
    Ok(())
}

async fn seed_prior_vocabulary(store: &SqliteStore, rows: &[RecordedVocab]) -> TestResult {
    for row in rows {
        if row.state == "suggested" {
            continue;
        }
        seed_vocab(store, row).await?;
    }
    Ok(())
}

async fn seed_vocab(store: &SqliteStore, row: &RecordedVocab) -> TestResult {
    let row = row.clone();
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO ros_tag_taxonomy \
                    (key, value, display, aliases, state, source, reason, decided_at, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    row.key,
                    row.value,
                    row.display,
                    row.aliases,
                    row.state,
                    row.source,
                    row.reason,
                    row.decided_at,
                    row.created_at,
                    row.updated_at
                ],
            )
            .map_err(common::seed)?;
            Ok(())
        })
        .await?;
    Ok(())
}

async fn seed_tag(store: &SqliteStore, row: &RecordedTag, clear_removal: bool) -> TestResult {
    let mut row = row.clone();
    if clear_removal {
        row.removal_state = None;
        row.removal_reason.clear();
    }
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO ros_tags \
                    (id, entity_type, entity_id, key, value, display, source, state, confidence, \
                     proposed_by, reason, decided_by, decided_at, created_at, updated_at, \
                     removal_state, removal_reason) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                params![
                    row.id,
                    row.entity_type,
                    row.entity_id,
                    row.key,
                    row.value,
                    row.display,
                    row.source,
                    row.state,
                    row.confidence,
                    row.proposed_by,
                    row.reason,
                    row.decided_by,
                    row.decided_at,
                    row.created_at,
                    row.updated_at,
                    row.removal_state,
                    row.removal_reason
                ],
            )
            .map_err(common::seed)?;
            Ok(())
        })
        .await?;
    Ok(())
}

async fn seed_plain(
    store: &SqliteStore,
    entity: Uuid,
    value: &str,
    state: &str,
    removal_state: Option<&str>,
    removal_reason: &str,
) -> TestResult {
    let id = Uuid::new_v4().to_string();
    let entity = entity.to_string();
    let value = value.to_owned();
    let state = state.to_owned();
    let removal_state = removal_state.map(str::to_owned);
    let removal_reason = removal_reason.to_owned();
    let at = common::RECORDED_START.to_owned();
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO ros_tags \
                    (id, entity_type, entity_id, key, value, display, source, state, \
                     proposed_by, reason, decided_by, decided_at, created_at, updated_at, \
                     removal_state, removal_reason) \
                 VALUES (?1, 'conversation', ?2, 'topic', ?3, ?3, 'user', ?4, 'owner', '', 'owner', ?5, ?5, ?5, ?6, ?7)",
                params![id, entity, value, state, at, removal_state, removal_reason],
            )
            .map_err(common::seed)?;
            Ok(())
        })
        .await?;
    Ok(())
}

async fn tag_id(store: &SqliteStore, entity: Uuid, key: &str, value: &str) -> TestResult<Uuid> {
    let entity = entity.to_string();
    let key = key.to_owned();
    let value = value.to_owned();
    let text = store
        .with_conn(move |conn| {
            let id: String = conn
                .query_row(
                    "SELECT id FROM ros_tags WHERE entity_id = ?1 AND key = ?2 AND value = ?3",
                    params![entity, key, value],
                    |row| row.get(0),
                )
                .map_err(common::seed)?;
            Ok(id)
        })
        .await?;
    Ok(Uuid::parse_str(&text)?)
}

async fn listed(
    store: &SqliteStore,
    entity: Uuid,
    key: &str,
    value: &str,
) -> TestResult<kernel::memory::TagWire> {
    let rows = store
        .list_tags(TagListQuery {
            entity_type: Some(TagEntityType::Summary),
            entity_id: Some(entity),
            key: Some(key.to_owned()),
            value: Some(value.to_owned()),
            states: Vec::new(),
            limit: kernel::memory::Limit::clamped(Some(20), 20, 1..=20),
        })
        .await?;
    rows.into_iter().next().ok_or_else(|| "listed tag".into())
}

fn add(
    entity_type: TagEntityType,
    entity_id: Uuid,
    key: &str,
    value: &str,
    display: &str,
    confidence: Option<f64>,
) -> TagSuggestionRow {
    TagSuggestionRow {
        kind: TagSuggestKind::Add,
        entity_type,
        entity_id,
        key: key.to_owned(),
        value: value.to_owned(),
        display: display.to_owned(),
        confidence,
        reason: None,
    }
}

fn remove(entity_id: Uuid, value: &str, reason: &str) -> TagSuggestionRow {
    TagSuggestionRow {
        kind: TagSuggestKind::Remove,
        entity_type: TagEntityType::Conversation,
        entity_id,
        key: "topic".to_owned(),
        value: value.to_owned(),
        display: value.to_owned(),
        confidence: None,
        reason: Some(reason.to_owned()),
    }
}

fn removal_of(rows: &[Json], value: &str) -> TestResult<(Option<String>, String)> {
    let row = rows
        .iter()
        .find(|row| row.get("value").and_then(Json::as_str) == Some(value))
        .ok_or("removal row")?;
    let state = match row.get("removal_state") {
        Some(Json::Null) | None => None,
        Some(Json::String(text)) => Some(text.clone()),
        Some(other) => return Err(format!("removal_state {other}").into()),
    };
    let reason = match row.get("removal_reason") {
        Some(Json::String(text)) => text.clone(),
        Some(other) => return Err(format!("removal_reason {other}").into()),
        None => return Err("missing removal_reason".into()),
    };
    Ok((state, reason))
}

fn message_of(result: Result<Option<kernel::memory::TagJobContext>, MemoryStoreError>) -> String {
    match result {
        Err(MemoryStoreError::InvalidRequest { message }) => message,
        other => format!("unexpected {other:?}"),
    }
}
