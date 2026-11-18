use chrono::Utc;
use kernel::memory::summary_reference::{
    PreparedSummaryRecord, PreparedSummaryTree, SummaryManifestEntry, SummaryReferenceError,
    embedding_input_hash,
};
use kernel::memory::{
    EmbedTable, EmbedTarget, EmbeddingStore, EmbeddingStoreError, StoredEmbedding, SummaryKind,
};
use kernel::tenancy::Scope;
use kernel::wiki::builder::{ExtractWiki, WikiBuilderStore, WikiRouting, WikiStoreError, WikiWork};
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn id(value: u128) -> Uuid {
    Uuid::from_u128(value)
}

fn tree(old: &[u128], root: u128, messages: &[u128], historical: bool) -> PreparedSummaryTree {
    PreparedSummaryTree {
        conversation: id(1),
        owner: "alice".to_owned(),
        expected_ids: old.iter().copied().map(id).collect(),
        expected_revision: None,
        historical,
        created_at: Utc::now(),
        records: vec![PreparedSummaryRecord {
            id: id(root),
            parent: None,
            kind: SummaryKind::Session,
            ordinal: 0,
            content: "The brass valve is sealed.".to_owned(),
            input_key: [4; 32],
            index_key: None,
            model: None,
            issued_part_ordinal: None,
            input_tokens: None,
            quality_flags: 0,
            section: None,
            sources: messages
                .iter()
                .copied()
                .map(id)
                .zip(1..)
                .map(|(message, ordinal)| SummaryManifestEntry {
                    message,
                    ordinal,
                    render_key: None,
                })
                .collect(),
        }],
    }
}

async fn seed(pool: &PgPool) -> TestResult {
    sqlx::raw_sql(
        "INSERT INTO rh_conversations (id, session_key, agent, channel, owner_user_id)
         VALUES ('00000000-0000-0000-0000-000000000001', 'valves', 'fixture', 'cli', 'alice');
         INSERT INTO rh_messages (id, conversation_id, agent, channel, role, content, created_at, owner_user_id)
         VALUES ('00000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000001',
                 'fixture', 'cli', 'user', 'Seal the brass valve.', '2024-01-01', 'alice'),
                ('00000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-000000000001',
                 'fixture', 'cli', 'user', 'Check the spare gasket.', '2024-01-02', 'alice');",
    ).execute(pool).await?;
    for (summary, message, accesses) in [(10, 2, 2), (11, 2, 3), (12, 3, 9)] {
        sqlx::query(
            "INSERT INTO rh_summaries (id, conversation_id, content, owner_user_id, access_count)
             VALUES ($1, $2, 'Earlier summary.', 'alice', $3)",
        )
        .bind(id(summary))
        .bind(id(1))
        .bind(accesses)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO rh_summary_sources (summary_id, message_id, ordinal) VALUES ($1, $2, 0)",
        )
        .bind(id(summary))
        .bind(id(message))
        .execute(pool)
        .await?;
    }
    sqlx::query("INSERT INTO rh_wiki_topics (slug, title, owner_user_id) VALUES ('valve', 'Valve', 'alice')")
        .execute(pool).await?;
    for (summary, note) in [(10, "first"), (11, "latest"), (12, "unmapped")] {
        sqlx::query(
            "INSERT INTO rh_wiki_citations (topic_slug, summary_id, kind, note, cited_at, owner_user_id)
             VALUES ('valve', $1, 'leaf', $2, '2024-01-01'::timestamptz + $3 * interval '1 hour', 'alice')",
        ).bind(id(summary)).bind(note).bind(i32::try_from(summary)?).execute(pool).await?;
    }
    sqlx::query(
        "INSERT INTO rh_wiki_extractions (summary_id, status, pipeline_version, owner_user_id)
         VALUES ($1, 'done', 4, 'alice')",
    )
    .bind(id(10))
    .execute(pool)
    .await?;
    Ok(())
}

async fn scoped_store(pool: &PgPool) -> Result<PostgresMemory, sqlx::Error> {
    let scoped = memory_postgres::scoped(sqlx::postgres::PgPoolOptions::new())
        .connect_with(pool.connect_options().as_ref().clone())
        .await?;
    Ok(PostgresMemory::new(scoped))
}

async fn checksum(pool: &PgPool) -> Result<String, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT md5(string_agg(to_jsonb(m)::text, ',' ORDER BY m.id)) FROM rh_messages m",
    )
    .fetch_one(pool)
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_references_swap_keeps_receipts_and_remaps_or_deletes_references(
    pool: PgPool,
) -> TestResult {
    seed(&pool).await?;
    let before = checksum(&pool).await?;
    let store = scoped_store(&pool).await?;
    let prepared = tree(&[10, 11, 12], 20, &[2], true);
    Scope::Maintenance
        .run(store.replace_summary_tree(&prepared))
        .await?;

    let citation: (Uuid, String, String) =
        sqlx::query_as("SELECT summary_id, kind, note FROM rh_wiki_citations")
            .fetch_one(&pool)
            .await?;
    assert_eq!(
        citation,
        (id(20), "session".to_owned(), "latest".to_owned())
    );
    let accesses: i32 = sqlx::query_scalar("SELECT access_count FROM rh_summaries WHERE id = $1")
        .bind(id(20))
        .fetch_one(&pool)
        .await?;
    assert_eq!(accesses, 5);
    let receipts: Vec<(Uuid, Option<i32>, bool)> = sqlx::query_as(
        "SELECT message_id, completed_version, historical_backlog FROM rh_summary_deliveries
         WHERE consumer = 'wiki' ORDER BY message_id",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(receipts, [(id(2), Some(4), false), (id(3), None, true)]);
    assert_eq!(before, checksum(&pool).await?);

    let gone = Scope::Maintenance
        .run(store.apply_wiki(
            &ExtractWiki {
                summary_id: id(10),
                conversation_id: Some(id(1)),
            },
            Vec::new(),
            &WikiRouting::default(),
            Utc::now(),
        ))
        .await;
    assert!(
        matches!(gone, Err(WikiStoreError::Missing(uuid) | WikiStoreError::Reference(SummaryReferenceError::Gone(uuid))) if uuid == id(10))
    );
    let embed = Scope::Maintenance
        .run(store.write(StoredEmbedding {
            target: EmbedTarget {
                table: EmbedTable::Summaries,
                id: id(10).to_string(),
            },
            vector: vec![1.; 1024],
            content_hash: embedding_input_hash("Earlier summary."),
            chunks: Vec::new(),
        }))
        .await;
    assert!(
        matches!(embed, Err(EmbeddingStoreError::SummaryReference(SummaryReferenceError::Gone(uuid))) if uuid == id(10))
    );
    Scope::Maintenance.run(check_gone(&store)).await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_references_wiki_acceptance_and_receipts_commit_together(
    pool: PgPool,
) -> TestResult {
    seed(&pool).await?;
    let directory = tempfile::tempdir()?;
    let store = scoped_store(&pool)
        .await?
        .with_wiki_directory(directory.path().to_path_buf());
    Scope::Maintenance
        .run(async {
            store
                .replace_summary_tree(&tree(&[10, 11, 12], 20, &[2, 3], false))
                .await?;
            let job = ExtractWiki {
                summary_id: id(20),
                conversation_id: Some(id(1)),
            };
            let WikiWork::Pending(input) = store.wiki_work(&job).await? else {
                return Err("expected pending work".into());
            };
            assert_eq!(input.delivery.as_ref().expect("v7 input").pending, [id(3)]);
            store
                .apply_wiki(
                    &job,
                    Vec::new(),
                    &WikiRouting {
                        input: Some(input),
                        ..WikiRouting::default()
                    },
                    Utc::now(),
                )
                .await?;
            store
                .replace_summary_tree(&tree(&[20], 21, &[2, 3], false))
                .await?;
            let work = store
                .wiki_work(&ExtractWiki {
                    summary_id: id(21),
                    ..job
                })
                .await?;
            assert!(matches!(work, WikiWork::Complete { .. }));
            Ok(())
        })
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_references_exclusive_waits_for_effects_and_revalidates(
    pool: PgPool,
) -> TestResult {
    use std::time::Duration;

    seed(&pool).await?;
    let store = scoped_store(&pool).await?;
    Scope::Maintenance.run(async {
        let lease = store.lease_summary_effects(&[id(10)], &[]).await?;
        let prepared = tree(&[10, 11, 12], 20, &[2], true);
        let replacement = store.replace_summary_tree(&prepared);
        tokio::pin!(replacement);
        assert!(tokio::time::timeout(Duration::from_millis(100), &mut replacement).await.is_err());

        sqlx::query(
            "INSERT INTO rh_summaries (id, conversation_id, content, owner_user_id)
             VALUES ($1, $2, 'A concurrent rollup.', 'alice')",
        ).bind(id(13)).bind(id(1)).execute(&pool).await?;
        drop(lease);
        let result = tokio::time::timeout(Duration::from_secs(5), replacement).await?;
        assert!(matches!(result, Err(memory_postgres::ReplaceSummaryError::Reference(SummaryReferenceError::Stale(uuid))) if uuid == id(1)));
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_summaries WHERE conversation_id = $1")
            .bind(id(1)).fetch_one(&pool).await?;
        assert_eq!(count, 4);
        Ok(())
    }).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_references_other_owner_cannot_see_a_fence_or_receipt(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let store = scoped_store(&pool).await?;
    Scope::Maintenance
        .run(store.replace_summary_tree(&tree(&[10, 11, 12], 20, &[2], true)))
        .await?;
    let probe = crate::common::probe(&pool).await?;
    let other = PostgresMemory::new(probe.clone());
    Scope::Viewer(crate::visibility::viewer("bob")?).run(async {
        let blocked = other.lease_summary_effects(&[id(20)], &[]).await;
        assert!(matches!(blocked, Err(WikiStoreError::Reference(SummaryReferenceError::Gone(uuid))) if uuid == id(20)));
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_summary_deliveries")
            .fetch_one(&probe).await?;
        assert_eq!(rows, 0);
        Ok(())
    }).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_references_entity_acceptance_is_receipted(pool: PgPool) -> TestResult {
    use kernel::wiki::entity_pass::{EntityStore, EntityWork, ExtractEntities};

    seed(&pool).await?;
    let store = scoped_store(&pool).await?;
    Scope::Maintenance
        .run(async {
            store
                .replace_summary_tree(&tree(&[10, 11, 12], 20, &[2, 3], false))
                .await?;
            let job = ExtractEntities {
                summary_id: id(20),
                conversation_id: Some(id(1)),
                session_at: None,
            };
            let EntityWork::Pending(leaf) = store.entity_work(&job).await? else {
                return Err("expected entity work".into());
            };
            assert_eq!(
                leaf.delivery.as_ref().expect("delivery").pending,
                [id(2), id(3)]
            );
            store
                .finish_entity_delivery(&[leaf], &[], None, Utc::now())
                .await?;
            store
                .replace_summary_tree(&tree(&[20], 21, &[2, 3], false))
                .await?;
            assert!(matches!(
                store
                    .entity_work(&ExtractEntities {
                        summary_id: id(21),
                        ..job
                    })
                    .await?,
                EntityWork::Complete { .. }
            ));
            Ok(())
        })
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_references_related_rows_follow_the_winner(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let entity: Uuid =
        sqlx::query_scalar("SELECT id FROM rh_topics WHERE kind = 'type' AND slug = 'hardware'")
            .fetch_one(&pool)
            .await?;
    let rows = [
        "INSERT INTO rh_wiki_provenance (topic_slug, source_kind, source_id, owner_user_id)
         SELECT 'valve', 'summary', id, owner_user_id FROM rh_summaries WHERE conversation_id = $1",
        "INSERT INTO rh_wiki_entity_candidates (key, source, summary_id, patch, held_at, owner_user_id)
         SELECT 'valve', 'summary:' || id, id, '{}', now(), owner_user_id FROM rh_summaries WHERE conversation_id = $1",
        "INSERT INTO rh_wiki_deferred (summary_id, waits_conversation, patches, deferred_at, owner_user_id)
         SELECT id, conversation_id, '[]', now(), owner_user_id FROM rh_summaries WHERE conversation_id = $1",
        "INSERT INTO rh_entity_extractions (summary_id, status, pipeline_version, entities_touched, extracted_at, owner_user_id)
         SELECT id, 'done', 1, ARRAY[id::text], now(), owner_user_id FROM rh_summaries WHERE conversation_id = $1",
        "INSERT INTO rh_entity_history (entity_id, summary_id, conversation_id, session_on, sentence, owner_user_id)
         SELECT $2, id, conversation_id, now()::date, id::text, owner_user_id FROM rh_summaries WHERE conversation_id = $1",
        "INSERT INTO rh_entity_type_seen (entity_id, summary_id, type_name, observed_at, owner_user_id)
         SELECT $2, id, 'Hardware', now(), owner_user_id FROM rh_summaries WHERE conversation_id = $1",
        "INSERT INTO rh_entity_decisions (id, summary_id, kind, proposed_name, proposed_type, candidates, verdict, model, created_at, owner_user_id)
         SELECT gen_random_uuid(), id, 'entity', 'Valve', 'Hardware', '[]', 'hold', 'fixture', now(), owner_user_id
         FROM rh_summaries WHERE conversation_id = $1",
        "INSERT INTO rh_entity_candidates (id, summary_id, name, type_name, confidence, reason, payload, created_at, owner_user_id)
         SELECT gen_random_uuid(), id, 'Valve', 'Hardware', 0.5, 'fixture', '{}', now(), owner_user_id
         FROM rh_summaries WHERE conversation_id = $1",
        "INSERT INTO rh_entity_applied (entity_id, applied_through, summary_id, owner_user_id)
         SELECT $2, now(), id, owner_user_id FROM rh_summaries WHERE conversation_id = $1 ORDER BY id DESC LIMIT 1",
        "INSERT INTO rh_tags (entity_type, entity_id, key, value, source, state)
         SELECT 'summary', id, 'type', 'hardware', 'user', 'accepted' FROM rh_summaries WHERE conversation_id = $1",
    ];
    for statement in rows {
        let query = sqlx::query(statement).bind(id(1));
        if statement.contains("$2") {
            query.bind(entity).execute(&pool).await?;
        } else {
            query.execute(&pool).await?;
        }
    }
    let fact: Uuid = sqlx::query_scalar(
        "INSERT INTO rh_entity_facts (id, topic_id, key, created_at) VALUES (gen_random_uuid(), $1, 'size', now()) RETURNING id",
    ).bind(entity).fetch_one(&pool).await?;
    sqlx::query(
        "INSERT INTO rh_entity_fact_claims (id, fact_id, value, actor, session_id, summary_id, at, recorded_at, owner_user_id)
         SELECT gen_random_uuid(), $2, id::text, 'wiki-writer', 'valves', id, now(), now(), owner_user_id
         FROM rh_summaries WHERE conversation_id = $1",
    ).bind(id(1)).bind(fact).execute(&pool).await?;
    sqlx::query(
        "SELECT graphile_worker.add_job('extract-wiki', json_build_object('summaryId', $1::text),
            job_key => 'wiki-ext-' || $1::text)",
    )
    .bind(id(12))
    .execute(&pool)
    .await?;

    let store = scoped_store(&pool).await?;
    Scope::Maintenance
        .run(store.replace_summary_tree(&tree(&[10, 11, 12], 20, &[2], true)))
        .await?;
    let references = [
        ("SELECT source_id FROM rh_wiki_provenance", 1),
        ("SELECT summary_id FROM rh_wiki_entity_candidates", 1),
        ("SELECT summary_id FROM rh_wiki_deferred", 1),
        ("SELECT summary_id FROM rh_entity_extractions", 1),
        ("SELECT summary_id FROM rh_entity_history", 1),
        ("SELECT summary_id FROM rh_entity_type_seen", 1),
        ("SELECT summary_id FROM rh_entity_decisions", 2),
        ("SELECT summary_id FROM rh_entity_candidates", 2),
        ("SELECT summary_id FROM rh_entity_applied", 1),
        (
            "SELECT entity_id FROM rh_tags WHERE entity_type = 'summary'",
            1,
        ),
        ("SELECT summary_id FROM rh_entity_fact_claims", 1),
    ];
    for (statement, count) in references {
        let found: Vec<Uuid> = sqlx::query_scalar(statement).fetch_all(&pool).await?;
        assert_eq!(found, vec![id(20); count], "{statement}");
    }
    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM graphile_worker._private_jobs
         WHERE payload->>'summaryId' IS NOT NULL OR payload->>'targetTable' = 'ros_summaries' ",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(jobs, 0);
    Ok(())
}

async fn check_gone(
    store: &(impl kernel::memory::MemoryStore + kernel::facts::FactStore),
) -> TestResult {
    use kernel::facts::{FactError, FactKey, FactValue, NewClaim, Said};
    use kernel::memory::{
        MemoryStoreError, TagEntityType, TagSuggestKind, TagSuggestionRow, TagSuggestionWrite,
    };

    let fact = store
        .assert_facts(&[NewClaim {
            entity: kernel::topics::TopicId::random(),
            key: FactKey::new("seal")?,
            value: FactValue::new("closed")?,
            said: Said::Writer {
                session: "claude-code:00000000-0000-4000-8000-000000000001".parse()?,
                summary: id(10),
                evidence: None,
                settled: false,
            },
            owner: None,
            at: Utc::now(),
        }])
        .await;
    assert!(
        matches!(fact, Err(FactError::SummaryReference(SummaryReferenceError::Gone(uuid))) if uuid == id(10))
    );

    let tags = store
        .store_tag_suggestions(TagSuggestionWrite {
            proposed_by: "fixture".to_owned(),
            at: Utc::now(),
            rows: vec![TagSuggestionRow {
                kind: TagSuggestKind::Add,
                entity_type: TagEntityType::Summary,
                entity_id: id(10),
                key: "material".to_owned(),
                value: "brass".to_owned(),
                display: "Brass".to_owned(),
                confidence: Some(0.8),
                reason: None,
            }],
            vocabulary: Vec::new(),
        })
        .await;
    assert!(
        matches!(tags, Err(MemoryStoreError::SummaryReference(SummaryReferenceError::Gone(uuid))) if uuid == id(10))
    );
    Ok(())
}
