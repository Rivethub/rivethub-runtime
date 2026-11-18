#![cfg(test)]

use jiff::Timestamp;
use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use kernel::memory::MemoryStore as _;
use kernel::memory::MemoryStoreError;
use kernel::memory::TagEntityType;
use kernel::memory::TagState;
use kernel::memory::TagSuggestKind;
use kernel::memory::TagSuggestionRow;
use kernel::memory::TagSuggestionStored;
use kernel::memory::TagSuggestionWrite;
use kernel::memory::TagVocabularyProposal;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use uuid::Uuid;
use wire::message::Role;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, sqlx::FromRow)]
struct StoredTag {
    display: String,
    source: String,
    state: String,
    confidence: Option<f64>,
    proposed_by: String,
    reason: String,
    removal_state: Option<String>,
    removal_reason: String,
}

#[derive(Debug, sqlx::FromRow)]
struct StoredVocab {
    display: String,
    state: String,
    source: String,
    reason: String,
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn tag_job_context_reads_the_summary(pool: PgPool) -> TestResult {
    let memory = store(&pool);
    let conversation = seed_conversation(&pool, "cli:s1").await?;
    sqlx::query("update rh_conversations set title = 'Deploy' where id = $1")
        .bind(conversation)
        .execute(&pool)
        .await?;
    let summary = id(20);
    insert_summary(&pool, summary, Some(conversation), "The team decided.").await?;
    insert_taxonomy(
        &pool,
        "topic",
        "memory-summarizer",
        "",
        "accepted",
        stamp(1),
    )
    .await?;
    insert_taxonomy(
        &pool,
        "project",
        "acmekit",
        "AcmeKit",
        "suggested",
        stamp(2),
    )
    .await?;
    insert_tag(
        &pool,
        tag_seed(
            "conversation",
            conversation,
            "topic",
            "manual",
            "user",
            None,
        ),
    )
    .await?;
    let loaded = memory.tag_job_context(summary).await?.expect("context");
    assert_eq!(loaded.summary_id, summary);
    assert_eq!(loaded.conversation_id, conversation);
    assert_eq!(loaded.session_key, "cli:s1");
    assert_eq!(loaded.title.as_deref(), Some("Deploy"));
    assert_eq!(loaded.agent, "rivet");
    assert_eq!(loaded.summary, "The team decided.");
    assert_eq!(loaded.vocabulary.len(), 3);
    let first = loaded.vocabulary.first().expect("first vocabulary");
    assert_eq!(first.key, "topic");
    assert_eq!(first.value, "memory-summarizer");
    assert_eq!(first.state, TagState::Accepted);
    let second = loaded.vocabulary.get(1).expect("second vocabulary");
    assert_eq!(second.value, "acmekit");
    assert_eq!(second.display, "AcmeKit");
    assert_eq!(second.state, TagState::Suggested);
    let third = loaded.vocabulary.get(2).expect("tag in use");
    assert_eq!(third.value, "manual");
    assert_eq!(third.state, TagState::Accepted);
    let tag = loaded.tags.first().expect("tag");
    assert_eq!(loaded.tags.len(), 1);
    assert_eq!(tag.entity_type, TagEntityType::Conversation);
    assert_eq!(tag.key, "topic");
    assert_eq!(tag.value, "manual");
    assert_eq!(tag.source, "user");
    assert_eq!(tag.state, TagState::Accepted);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn the_vocabulary_adds_accepted_tags_in_use(pool: PgPool) -> TestResult {
    let memory = store(&pool);
    let conversation = seed_conversation(&pool, "cli:vocab").await?;
    let summary = id(22);
    insert_summary(&pool, summary, Some(conversation), "body").await?;
    insert_taxonomy(&pool, "project", "scratch", "", "rejected", stamp(1)).await?;
    insert_taxonomy(&pool, "topic", "storage", "", "accepted", stamp(2)).await?;
    let other = seed_conversation(&pool, "cli:other").await?;
    for (key, value, state) in [
        ("project", "Widget Hub", "accepted"),
        ("project", "scratch", "accepted"),
        ("topic", "storage", "accepted"),
        ("topic", "maybe", "suggested"),
    ] {
        let mut seed = tag_seed("conversation", other, key, value, "user", None);
        seed.state = state;
        insert_tag(&pool, seed).await?;
    }
    let loaded = memory.tag_job_context(summary).await?.expect("context");
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

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn suggestions_do_not_reset_an_accepted_tag(pool: PgPool) -> TestResult {
    let memory = store(&pool);
    let conversation = seed_conversation(&pool, "cli:accepted").await?;
    let summary = id(21);
    insert_summary(&pool, summary, Some(conversation), "body").await?;
    insert_tag(
        &pool,
        tag_seed("summary", summary, "project", "acmekit", "user", None),
    )
    .await?;
    insert_taxonomy(&pool, "project", "acmekit", "AcmeKit", "accepted", stamp(1)).await?;
    let stored = memory
        .store_tag_suggestions(write(
            vec![
                add(
                    TagEntityType::Summary,
                    summary,
                    "project",
                    "acmekit",
                    "Changed",
                    Some(0.9),
                ),
                add(
                    TagEntityType::Summary,
                    summary,
                    "topic",
                    "release-notes",
                    "",
                    Some(0.8),
                ),
                add(
                    TagEntityType::Conversation,
                    conversation,
                    "topic",
                    "release-notes",
                    "",
                    Some(0.8),
                ),
            ],
            vec![
                proposal("project", "acmekit", "AcmeKit"),
                proposal("topic", "release-notes", ""),
            ],
        ))
        .await?;
    assert_eq!(
        stored,
        TagSuggestionStored {
            tags: 2,
            removals: 0,
            vocabulary: 1
        }
    );
    let kept = fetch_tag(&pool, summary, "project", "acmekit").await?;
    assert_eq!(kept.source, "user");
    assert_eq!(kept.state, "accepted");
    assert_eq!(kept.display, "acmekit");
    let fresh = fetch_tag(&pool, summary, "topic", "release-notes").await?;
    assert_eq!(fresh.source, "model");
    assert_eq!(fresh.state, "suggested");
    assert_eq!(fresh.display, "");
    assert_eq!(fresh.proposed_by, "tagger-v1");
    assert_eq!(fresh.reason, "");
    assert_eq!(fresh.confidence.map(f64::to_bits), Some(0.8_f64.to_bits()));
    let unchanged = fetch_vocab(&pool, "project", "acmekit").await?;
    assert_eq!(unchanged.state, "accepted");
    assert_eq!(unchanged.source, "user");
    let created = fetch_vocab(&pool, "topic", "release-notes").await?;
    assert_eq!(created.state, "suggested");
    assert_eq!(created.source, "model");
    assert_eq!(created.display, "");
    assert_eq!(created.reason, "proposed by tagger-v1");
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn removals_fill_only_a_null_state(pool: PgPool) -> TestResult {
    let memory = store(&pool);
    let conversation = seed_conversation(&pool, "cli:removals").await?;
    insert_tag(
        &pool,
        tag_seed(
            "conversation",
            conversation,
            "topic",
            "manual",
            "user",
            None,
        ),
    )
    .await?;
    insert_tag(
        &pool,
        tag_seed(
            "conversation",
            conversation,
            "topic",
            "kept",
            "user",
            Some("rejected"),
        ),
    )
    .await?;
    insert_tag(
        &pool,
        tag_seed(
            "conversation",
            conversation,
            "topic",
            "already",
            "model",
            Some("suggested"),
        ),
    )
    .await?;
    let stored = memory
        .store_tag_suggestions(write(
            vec![
                remove(conversation, "topic", "manual", "allowed now"),
                remove(conversation, "topic", "kept", "nope"),
                remove(conversation, "topic", "already", "again"),
            ],
            Vec::new(),
        ))
        .await?;
    assert_eq!(stored.tags, 0);
    assert_eq!(stored.removals, 1);
    let manual = fetch_tag(&pool, conversation, "topic", "manual").await?;
    assert_eq!(manual.removal_state.as_deref(), Some("suggested"));
    assert_eq!(manual.removal_reason, "allowed now");
    assert_eq!(manual.state, "accepted");
    let kept = fetch_tag(&pool, conversation, "topic", "kept").await?;
    assert_eq!(kept.removal_state.as_deref(), Some("rejected"));
    let already = fetch_tag(&pool, conversation, "topic", "already").await?;
    assert_eq!(already.removal_state.as_deref(), Some("suggested"));
    assert_eq!(already.removal_reason, "");
    let again = memory
        .store_tag_suggestions(write(
            vec![remove(conversation, "topic", "manual", "twice")],
            Vec::new(),
        ))
        .await?;
    assert_eq!(again.removals, 0);
    let still = fetch_tag(&pool, conversation, "topic", "manual").await?;
    assert_eq!(still.removal_reason, "allowed now");
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn missing_summaries_and_missing_tables_fail_closed(pool: PgPool) -> TestResult {
    let memory = store(&pool);
    assert!(memory.tag_job_context(id(30)).await?.is_none());
    let bare = id(31);
    insert_summary(&pool, bare, None, "no conversation").await?;
    let orphan = memory.tag_job_context(bare).await;
    assert!(matches!(
        orphan,
        Err(MemoryStoreError::InvalidRequest { .. })
    ));
    let conversation = seed_conversation(&pool, "cli:missing-tables").await?;
    let summary = id(32);
    insert_summary(&pool, summary, Some(conversation), "body").await?;
    sqlx::query("drop table rh_tag_taxonomy cascade")
        .execute(&pool)
        .await?;
    sqlx::query("drop table rh_tags cascade")
        .execute(&pool)
        .await?;
    let context = memory.tag_job_context(summary).await;
    assert!(matches!(context, Err(MemoryStoreError::TagTablesMissing)));
    let stored = memory
        .store_tag_suggestions(write(
            vec![add(
                TagEntityType::Summary,
                summary,
                "topic",
                "notes",
                "",
                None,
            )],
            Vec::new(),
        ))
        .await;
    assert!(matches!(stored, Err(MemoryStoreError::TagTablesMissing)));
    Ok(())
}

fn store(pool: &PgPool) -> PostgresMemory {
    PostgresMemory::new(pool.clone())
}

async fn seed_conversation(
    pool: &PgPool,
    session: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory = store(pool);
    let batch = CaptureBatchRequest {
        session_key: session.to_owned(),
        agent: "rivet".to_owned(),
        channel: Some("claude-code".to_owned()),
        title: None,
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages: vec![CaptureMessage {
            event_id: "only".to_owned(),
            role: Role::User,
            content: "tagged turn".to_owned(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            metadata: None,
            created_at: None,
        }],
    };
    memory
        .capture_batch(
            &batch,
            &memory_postgres::CaptureOptions::owned_by(&testkit::owner()?),
        )
        .await?;
    let found: Uuid = sqlx::query_scalar("select id from rh_conversations where session_key = $1")
        .bind(session)
        .fetch_one(pool)
        .await?;
    Ok(found)
}

async fn insert_summary(
    pool: &PgPool,
    summary: Uuid,
    conversation: Option<Uuid>,
    content: &str,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("select set_config('rivet.defer_embed_enqueue', 'on', true)")
        .execute(&mut *tx)
        .await?;
    sqlx::query("insert into rh_summaries (id, conversation_id, content, owner_user_id) values ($1, $2, $3, 'owner')")
        .bind(summary)
        .bind(conversation)
        .bind(content)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

struct TagSeed<'a> {
    entity_type: &'a str,
    entity_id: Uuid,
    key: &'a str,
    value: &'a str,
    display: &'a str,
    source: &'a str,
    state: &'a str,
    removal_state: Option<&'a str>,
}

fn tag_seed<'a>(
    entity_type: &'a str,
    entity_id: Uuid,
    key: &'a str,
    value: &'a str,
    source: &'a str,
    removal_state: Option<&'a str>,
) -> TagSeed<'a> {
    TagSeed {
        entity_type,
        entity_id,
        key,
        value,
        display: value,
        source,
        state: "accepted",
        removal_state,
    }
}

async fn insert_tag(pool: &PgPool, seed: TagSeed<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into rh_tags (entity_type, entity_id, key, value, display, source, state, \
         removal_state, removal_reason) values ($1, $2, $3, $4, $5, $6, $7, $8, '')",
    )
    .bind(seed.entity_type)
    .bind(seed.entity_id)
    .bind(seed.key)
    .bind(seed.value)
    .bind(seed.display)
    .bind(seed.source)
    .bind(seed.state)
    .bind(seed.removal_state)
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert_taxonomy(
    pool: &PgPool,
    key: &str,
    value: &str,
    display: &str,
    state: &str,
    created_at: Timestamp,
) -> Result<(), sqlx::Error> {
    let aliases: Vec<String> = Vec::new();
    let source = if state == "accepted" { "user" } else { "model" };
    sqlx::query(
        "insert into rh_tag_taxonomy (key, value, display, aliases, state, source, created_at) \
         values ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(key)
    .bind(value)
    .bind(display)
    .bind(&aliases)
    .bind(state)
    .bind(source)
    .bind(memory_postgres::time::PgTimestamp::from(created_at))
    .execute(pool)
    .await?;
    Ok(())
}

async fn fetch_tag(
    pool: &PgPool,
    entity: Uuid,
    key: &str,
    value: &str,
) -> Result<StoredTag, sqlx::Error> {
    sqlx::query_as(
        "select display, source, state, confidence, proposed_by, reason, removal_state, \
         removal_reason from rh_tags where entity_id = $1 and key = $2 and value = $3",
    )
    .bind(entity)
    .bind(key)
    .bind(value)
    .fetch_one(pool)
    .await
}

async fn fetch_vocab(pool: &PgPool, key: &str, value: &str) -> Result<StoredVocab, sqlx::Error> {
    sqlx::query_as(
        "select display, state, source, reason from rh_tag_taxonomy where key = $1 and value = $2",
    )
    .bind(key)
    .bind(value)
    .fetch_one(pool)
    .await
}

fn write(
    rows: Vec<TagSuggestionRow>,
    vocabulary: Vec<TagVocabularyProposal>,
) -> TagSuggestionWrite {
    TagSuggestionWrite {
        proposed_by: "tagger-v1".to_owned(),
        at: stamp(10),
        rows,
        vocabulary,
    }
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

fn remove(entity_id: Uuid, key: &str, value: &str, reason: &str) -> TagSuggestionRow {
    TagSuggestionRow {
        kind: TagSuggestKind::Remove,
        entity_type: TagEntityType::Conversation,
        entity_id,
        key: key.to_owned(),
        value: value.to_owned(),
        display: value.to_owned(),
        confidence: None,
        reason: Some(reason.to_owned()),
    }
}

fn proposal(key: &str, value: &str, display: &str) -> TagVocabularyProposal {
    TagVocabularyProposal {
        key: key.to_owned(),
        value: value.to_owned(),
        display: display.to_owned(),
        reason: "proposed by tagger-v1".to_owned(),
    }
}

fn id(suffix: u8) -> Uuid {
    Uuid::parse_str(&format!("00000000-0000-4000-8000-{suffix:012x}")).expect("uuid")
}

fn stamp(second: i8) -> Timestamp {
    wire::time::timestamp_at_utc(2026, 10, 10, 12, 0, second).expect("time")
}
