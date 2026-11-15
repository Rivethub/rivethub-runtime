use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use kernel::memory::Limit;
use kernel::memory::MemoryStore as _;
use kernel::memory::MemoryStoreError;
use kernel::memory::TagAdd;
use kernel::memory::TagCountsQuery;
use kernel::memory::TagDecision;
use kernel::memory::TagEntity;
use kernel::memory::TagEntityType;
use kernel::memory::TagEntryPair;
use kernel::memory::TagId;
use kernel::memory::TagListQuery;
use kernel::memory::TagLookup;
use kernel::memory::TagState;
use kernel::memory::TagTaxonomyDecision;
use kernel::memory::TagTaxonomyMerge;
use kernel::memory::TagTaxonomyQuery;
use kernel::memory::TagTaxonomyUpsert;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use sqlx::Row as _;
use std::collections::BTreeMap;
use wire::message::Role;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn store(pool: &PgPool) -> PostgresMemory {
    PostgresMemory::new(pool.clone())
}

async fn seed_conversation(
    pool: &PgPool,
    session: &str,
) -> Result<sqlx::types::Uuid, Box<dyn std::error::Error>> {
    let memory = store(pool);
    let batch = CaptureBatchRequest {
        session_key: session.to_string(),
        agent: "rivet".to_string(),
        channel: Some("claude-code".to_string()),
        title: None,
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages: vec![CaptureMessage {
            event_id: "only".to_string(),
            role: Role::User,
            content: "tagged turn".to_string(),
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
    let id: sqlx::types::Uuid =
        sqlx::query_scalar("SELECT id FROM rh_conversations WHERE session_key = $1")
            .bind(session)
            .fetch_one(pool)
            .await?;
    Ok(id)
}

async fn insert_tag(
    pool: &PgPool,
    conversation: sqlx::types::Uuid,
    key: &str,
    value: &str,
    state: &str,
    source: &str,
) -> Result<sqlx::types::Uuid, Box<dyn std::error::Error>> {
    let id: sqlx::types::Uuid = sqlx::query_scalar(
        "INSERT INTO rh_tags (entity_type, entity_id, key, value, display, source, state)
         VALUES ('conversation', $1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(conversation)
    .bind(key)
    .bind(value)
    .bind(value)
    .bind(source)
    .bind(state)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

fn list_query(limit: u32) -> TagListQuery {
    TagListQuery {
        entity_type: None,
        entity_id: None,
        key: None,
        value: None,
        states: Vec::new(),
        limit: Limit::clamped(Some(limit), 200, 1..=200),
    }
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn add_tag_accepts_ids_and_session_keys(pool: PgPool) -> TestResult {
    let memory = store(&pool);
    let conversation = seed_conversation(&pool, "claude-code:tags").await?;

    let added = memory
        .add_tag(TagAdd {
            entity_type: TagEntityType::Conversation,
            entity: TagEntity::SessionKey("claude-code:tags".to_string()),
            agent: None,
            tag: kernel::memory::TagLiteral {
                key: "topic".to_string(),
                value: "deploy".to_string(),
            },
            display: Some("Deploy".to_string()),
            reason: Some("user picked it".to_string()),
            decided_by: Some("phil".to_string()),
        })
        .await?;
    assert_eq!(added.state, TagState::Accepted);
    assert_eq!(added.source, "user");
    assert_eq!(added.display, "Deploy");
    assert_eq!(added.decided_by.as_deref(), Some("phil"));
    assert!(
        added.decided_at.is_some(),
        "a decided add stamps decided_at"
    );
    assert_eq!(added.entity_id, conversation.to_string());

    let conflict = memory
        .add_tag(TagAdd {
            entity_type: TagEntityType::Conversation,
            entity: TagEntity::Id(conversation),
            agent: None,
            tag: kernel::memory::TagLiteral {
                key: "topic".to_string(),
                value: "deploy".to_string(),
            },
            display: None,
            reason: None,
            decided_by: None,
        })
        .await?;
    assert_eq!(conflict.id, added.id, "the unique pair is reused");
    assert_eq!(conflict.state, TagState::Accepted, "an add revives the row");

    sqlx::query(
        "INSERT INTO rh_conversations (session_key, agent, owner_user_id) VALUES ('claude-code:tags', 'other', 'owner')",
    )
    .execute(&pool)
    .await?;
    let several = memory
        .add_tag(TagAdd {
            entity_type: TagEntityType::Conversation,
            entity: TagEntity::SessionKey("claude-code:tags".to_string()),
            agent: None,
            tag: kernel::memory::TagLiteral {
                key: "topic".to_string(),
                value: "other".to_string(),
            },
            display: None,
            reason: None,
            decided_by: None,
        })
        .await;
    assert!(
        matches!(&several, Err(MemoryStoreError::InvalidRequest { message })
            if message.contains("exists under several agents")),
        "ambiguous sessions are refused: {several:?}"
    );

    let missing = memory
        .add_tag(TagAdd {
            entity_type: TagEntityType::Conversation,
            entity: TagEntity::SessionKey("claude-code:nobody".to_string()),
            agent: None,
            tag: kernel::memory::TagLiteral {
                key: "topic".to_string(),
                value: "other".to_string(),
            },
            display: None,
            reason: None,
            decided_by: None,
        })
        .await;
    assert!(
        matches!(&missing, Err(MemoryStoreError::InvalidRequest { message })
            if message.contains("no conversation captured")),
        "unknown sessions are refused: {missing:?}"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn add_tag_defaults_display_to_the_value(pool: PgPool) -> TestResult {
    let memory = store(&pool);
    let conversation = seed_conversation(&pool, "claude-code:display").await?;

    let omitted = memory
        .add_tag(TagAdd {
            entity_type: TagEntityType::Conversation,
            entity: TagEntity::Id(conversation),
            agent: None,
            tag: kernel::memory::TagLiteral {
                key: "project".to_string(),
                value: "General".to_string(),
            },
            display: None,
            reason: None,
            decided_by: None,
        })
        .await?;
    assert_eq!(
        omitted.display, "General",
        "an omitted display falls back to the value"
    );

    let blank = memory
        .add_tag(TagAdd {
            entity_type: TagEntityType::Conversation,
            entity: TagEntity::Id(conversation),
            agent: None,
            tag: kernel::memory::TagLiteral {
                key: "project".to_string(),
                value: "Local Model Dev".to_string(),
            },
            display: Some("   ".to_string()),
            reason: None,
            decided_by: None,
        })
        .await?;
    assert_eq!(
        blank.display, "Local Model Dev",
        "a blank display falls back to the value"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn list_pending_counts_and_lookup_read_the_tags(pool: PgPool) -> TestResult {
    let memory = store(&pool);
    let conversation = seed_conversation(&pool, "claude-code:read").await?;
    insert_tag(
        &pool,
        conversation,
        "project",
        "acmekit",
        "accepted",
        "rule",
    )
    .await?;
    insert_tag(&pool, conversation, "topic", "deploy", "suggested", "model").await?;
    let removal = insert_tag(&pool, conversation, "topic", "old", "accepted", "model").await?;
    sqlx::query(
        "UPDATE rh_tags SET removal_state = 'suggested', removal_reason = 'stale' WHERE id = $1",
    )
    .bind(removal)
    .execute(&pool)
    .await?;

    let listed = memory.list_tags(list_query(200)).await?;
    assert_eq!(listed.len(), 3);

    let mut accepted_only = list_query(200);
    accepted_only.states = vec![TagState::Accepted];
    assert_eq!(memory.list_tags(accepted_only).await?.len(), 2);

    let mut key_filter = list_query(200);
    key_filter.key = Some("project".to_string());
    let project_tags = memory.list_tags(key_filter).await?;
    assert_eq!(project_tags.len(), 1);
    assert_eq!(
        project_tags.first().map(|tag| tag.value.as_str()),
        Some("acmekit")
    );

    let pending = memory
        .pending_tags(Limit::clamped(Some(50), 50, 1..=200))
        .await?;
    assert_eq!(pending.len(), 2);
    let actions: Vec<(&str, &str)> = pending
        .iter()
        .map(|tag| {
            let action = match tag.action {
                Some(kernel::memory::PendingTagAction::Remove) => "remove",
                _ => "add",
            };
            (tag.tag.value.as_str(), action)
        })
        .collect();
    assert!(actions.contains(&("deploy", "add")), "{actions:?}");
    assert!(actions.contains(&("old", "remove")), "{actions:?}");
    let removal_wire = pending
        .iter()
        .find(|tag| tag.tag.value == "old")
        .ok_or("the removal suggestion is listed")?;
    assert_eq!(
        removal_wire.session_key.as_deref(),
        Some("claude-code:read")
    );
    assert_eq!(
        removal_wire.conversation_id.as_deref(),
        Some(conversation.to_string().as_str())
    );

    let counts = memory
        .tag_counts(TagCountsQuery {
            key: None,
            limit: Limit::clamped(Some(200), 200, 1..=200),
        })
        .await?;
    assert_eq!(counts.len(), 2, "only accepted tags are counted");
    let acme_count = counts.iter().find(|count| count.value == "acmekit");
    assert_eq!(acme_count.map(|count| count.conversations), Some(1));

    let lookup = memory
        .lookup_tags(TagLookup {
            session_keys: vec![
                "claude-code:read".to_string(),
                "claude-code:none".to_string(),
            ],
            states: Vec::new(),
        })
        .await?;
    let tagged = lookup.get("claude-code:read");
    assert_eq!(tagged.map(Vec::len), Some(3));
    assert!(!lookup.contains_key("claude-code:none"));

    let accepted_lookup = TagLookup {
        session_keys: vec!["claude-code:read".to_string()],
        states: vec![TagState::Accepted],
    };
    let accepted = memory.lookup_tags(accepted_lookup).await?;
    assert_eq!(accepted.get("claude-code:read").map(Vec::len), Some(2));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn decide_tags_updates_exactly_the_given_ids(pool: PgPool) -> TestResult {
    let memory = store(&pool);
    let conversation = seed_conversation(&pool, "claude-code:decide").await?;
    let first = insert_tag(&pool, conversation, "topic", "one", "suggested", "rule").await?;
    let second = insert_tag(&pool, conversation, "topic", "two", "suggested", "model").await?;

    let changed = memory
        .decide_tags(
            vec![TagId::from_uuid(first)],
            TagDecision::Accept,
            Some("phil".to_string()),
        )
        .await?;
    assert_eq!(changed, 1, "only the requested id changes");

    let mut decided_rows = sqlx::query(
        "SELECT state::text AS state, source::text AS source, decided_by, decided_at IS NOT NULL AS stamped
          FROM rh_tags WHERE id = $1",
    )
    .bind(first)
    .fetch_all(&pool)
    .await?
    .into_iter();
    let first_row = decided_rows.next().ok_or("first row missing")?;
    assert_eq!(first_row.try_get::<String, _>("state").unwrap(), "accepted");
    assert_eq!(
        first_row.try_get::<String, _>("source").unwrap(),
        "user",
        "a rule tag becomes user-owned"
    );
    assert_eq!(
        first_row.try_get::<String, _>("decided_by").unwrap(),
        "phil"
    );
    assert!(first_row.try_get::<bool, _>("stamped").unwrap());

    let second_row =
        sqlx::query("SELECT state::text AS state, decided_by FROM rh_tags WHERE id = $1")
            .bind(second)
            .fetch_one(&pool)
            .await?;
    assert_eq!(
        second_row.try_get::<String, _>("state").unwrap(),
        "suggested"
    );
    assert!(
        second_row
            .try_get::<Option<String>, _>("decided_by")
            .unwrap()
            .is_none()
    );

    let rejected = memory
        .decide_tags(vec![TagId::from_uuid(second)], TagDecision::Reject, None)
        .await?;
    assert_eq!(rejected, 1);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn taxonomy_upsert_decide_and_merge(pool: PgPool) -> TestResult {
    let memory = store(&pool);
    let conversation = seed_conversation(&pool, "claude-code:taxonomy").await?;
    insert_tag(&pool, conversation, "area", "legacy", "accepted", "model").await?;
    insert_tag(&pool, conversation, "area", "current", "accepted", "model").await?;
    let other = seed_conversation(&pool, "claude-code:taxonomy-two").await?;
    insert_tag(&pool, other, "area", "legacy", "accepted", "model").await?;

    let upserted = memory
        .upsert_tag_taxonomy(TagTaxonomyUpsert {
            key: "area".to_string(),
            value: "current".to_string(),
            display: Some("Current area".to_string()),
            parent_value: None,
            aliases: vec!["cur".to_string()],
            reason: Some("canonical term".to_string()),
        })
        .await?;
    assert_eq!(upserted.state, TagState::Accepted);
    assert_eq!(upserted.aliases, ["cur"]);

    let renamed = memory
        .upsert_tag_taxonomy(TagTaxonomyUpsert {
            key: "area".to_string(),
            value: "current".to_string(),
            display: Some("Current".to_string()),
            parent_value: None,
            aliases: Vec::new(),
            reason: None,
        })
        .await?;
    assert_eq!(renamed.display, "Current", "the upsert replaces the entry");

    let self_parent = memory
        .upsert_tag_taxonomy(TagTaxonomyUpsert {
            key: "area".to_string(),
            value: "loop".to_string(),
            display: None,
            parent_value: Some("loop".to_string()),
            aliases: Vec::new(),
            reason: None,
        })
        .await;
    assert!(
        matches!(self_parent, Err(MemoryStoreError::InvalidRequest { .. })),
        "a self parent violates the taxonomy check: {self_parent:?}"
    );

    let changed = memory
        .decide_tag_taxonomy(TagTaxonomyDecision {
            entries: vec![TagEntryPair {
                key: "area".to_string(),
                value: "current".to_string(),
            }],
            decision: TagDecision::Reject,
            decided_by: Some("phil".to_string()),
        })
        .await?;
    assert_eq!(changed, 1);

    let merged = memory
        .merge_tag_taxonomy(TagTaxonomyMerge {
            key: "area".to_string(),
            from: "legacy".to_string(),
            into: "current".to_string(),
        })
        .await?;
    assert_eq!(
        merged.dropped, 1,
        "the entity already holding the target value drops the duplicate"
    );
    assert_eq!(merged.moved, 1, "the other entity is re-pointed");
    assert_eq!(merged.into, "current");

    let legacy = sqlx::query(
        "SELECT count(*)::bigint AS n FROM rh_tags WHERE key = 'area' AND value = 'legacy'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        legacy.try_get::<i64, _>("n").unwrap(),
        0,
        "the merged-away value is gone"
    );
    let taxonomy_left = sqlx::query(
        "SELECT count(*)::bigint AS n FROM rh_tag_taxonomy WHERE key = 'area' AND value = 'legacy'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        taxonomy_left.try_get::<i64, _>("n").unwrap(),
        0,
        "the taxonomy entry is removed"
    );
    let alias =
        sqlx::query("SELECT aliases FROM rh_tag_taxonomy WHERE key = 'area' AND value = 'current'")
            .fetch_one(&pool)
            .await?;
    let aliases: Vec<String> = alias.try_get("aliases").unwrap();
    assert!(
        aliases.contains(&"legacy".to_string()),
        "the old value survives as an alias"
    );

    let duplicate = memory
        .merge_tag_taxonomy(TagTaxonomyMerge {
            key: "area".to_string(),
            from: "legacy".to_string(),
            into: "current".to_string(),
        })
        .await?;
    assert_eq!(duplicate.moved, 0);
    assert_eq!(duplicate.dropped, 0);

    let entries = memory
        .list_tag_taxonomy(TagTaxonomyQuery {
            key: Some("area".to_string()),
            state: None,
            limit: Limit::clamped(Some(500), 500, 1..=500),
        })
        .await?;
    assert_eq!(entries.len(), 1);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn pre_0019_databases_get_empty_reads_and_refused_writes(pool: PgPool) -> TestResult {
    let memory = store(&pool);
    sqlx::query("DROP TABLE rh_tag_taxonomy CASCADE")
        .execute(&pool)
        .await?;
    sqlx::query("DROP TABLE rh_tags CASCADE")
        .execute(&pool)
        .await?;

    assert_eq!(memory.list_tags(list_query(200)).await?, Vec::new());
    assert_eq!(
        memory
            .pending_tags(Limit::clamped(Some(50), 50, 1..=200))
            .await?,
        Vec::new()
    );
    assert_eq!(
        memory
            .tag_counts(TagCountsQuery {
                key: None,
                limit: Limit::clamped(Some(200), 200, 1..=200)
            })
            .await?,
        Vec::new()
    );
    assert_eq!(
        memory
            .list_tag_taxonomy(TagTaxonomyQuery {
                key: None,
                state: None,
                limit: Limit::clamped(Some(500), 500, 1..=500)
            })
            .await?,
        Vec::new()
    );
    assert_eq!(
        memory
            .lookup_tags(TagLookup {
                session_keys: vec!["claude-code:any".to_string()],
                states: Vec::new(),
            })
            .await?,
        BTreeMap::new()
    );

    let refused = memory
        .add_tag(TagAdd {
            entity_type: TagEntityType::Conversation,
            entity: TagEntity::Id(uuid::Uuid::nil()),
            agent: None,
            tag: kernel::memory::TagLiteral {
                key: "topic".to_string(),
                value: "deploy".to_string(),
            },
            display: None,
            reason: None,
            decided_by: None,
        })
        .await;
    assert!(matches!(refused, Err(MemoryStoreError::TagTablesMissing)));

    let refused = memory
        .decide_tags(Vec::new(), TagDecision::Accept, None)
        .await;
    assert!(matches!(refused, Err(MemoryStoreError::TagTablesMissing)));

    let refused = memory
        .upsert_tag_taxonomy(TagTaxonomyUpsert {
            key: "area".to_string(),
            value: "current".to_string(),
            display: None,
            parent_value: None,
            aliases: Vec::new(),
            reason: None,
        })
        .await;
    assert!(matches!(refused, Err(MemoryStoreError::TagTablesMissing)));

    let refused = memory
        .decide_tag_taxonomy(TagTaxonomyDecision {
            entries: vec![TagEntryPair {
                key: "area".to_string(),
                value: "current".to_string(),
            }],
            decision: TagDecision::Accept,
            decided_by: None,
        })
        .await;
    assert!(matches!(refused, Err(MemoryStoreError::TagTablesMissing)));

    let refused = memory
        .merge_tag_taxonomy(TagTaxonomyMerge {
            key: "area".to_string(),
            from: "legacy".to_string(),
            into: "current".to_string(),
        })
        .await;
    assert!(matches!(refused, Err(MemoryStoreError::TagTablesMissing)));
    Ok(())
}
