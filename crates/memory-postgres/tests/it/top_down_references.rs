use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;
use kernel::memory::summary_reference::{
    PreparedSummaryRecord, PreparedSummaryTree, SummaryManifestEntry, SummaryReferenceError,
    SummarySectionBounds, embedding_input_hash,
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
        created_at: Timestamp::now(),
        retained_parts: BTreeSet::new(),
        part_order: Vec::new(),
        passage_windows: BTreeMap::new(),
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

fn part(root: u128, node: u128, message: u128, ordinal: i32) -> PreparedSummaryRecord {
    PreparedSummaryRecord {
        id: id(node),
        parent: Some(id(root)),
        kind: SummaryKind::Part,
        ordinal,
        content: "A contained part.".to_owned(),
        input_key: [8; 32],
        index_key: None,
        model: None,
        issued_part_ordinal: Some(ordinal),
        input_tokens: Some(30),
        quality_flags: 0,
        section: None,
        sources: vec![SummaryManifestEntry {
            message: id(message),
            ordinal: 1,
            render_key: Some([9; 32]),
        }],
    }
}

fn section(root: u128, node: u128, ordinal: i32, tag: i32, text: &str) -> PreparedSummaryRecord {
    PreparedSummaryRecord {
        id: id(node),
        parent: Some(id(root)),
        kind: SummaryKind::Section,
        ordinal,
        content: text.to_owned(),
        input_key: [7; 32],
        index_key: None,
        model: None,
        issued_part_ordinal: None,
        input_tokens: None,
        quality_flags: 0,
        section: Some(SummarySectionBounds {
            first: tag,
            last: tag,
            verified: true,
            char_start: ordinal * 6,
            char_end: ordinal * 6 + 6,
        }),
        sources: Vec::new(),
    }
}

fn initial_parts() -> PreparedSummaryTree {
    let mut initial = tree(&[10, 11, 12], 20, &[2, 3], false);
    initial.records.extend([
        part(20, 21, 2, 1),
        part(20, 22, 3, 2),
        section(20, 23, 0, 1, "Valve."),
        section(20, 24, 1, 2, "Shelf."),
    ]);
    initial
}

fn reopen_parts() -> PreparedSummaryTree {
    let mut next = tree(&[20, 21, 22, 23, 24], 30, &[4, 2, 3], false);
    next.retained_parts.extend([id(21), id(22)]);
    next.part_order = vec![id(31), id(21), id(22)];
    next.records.extend([
        part(30, 21, 2, 1),
        part(30, 22, 3, 2),
        part(30, 31, 4, 3),
        section(30, 32, 0, 2, "Valve."),
        section(30, 33, 1, 3, "Stand."),
    ]);
    next
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
            Timestamp::now(),
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
                    Timestamp::now(),
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
                .finish_entity_delivery(&[leaf], &[], None, Timestamp::now())
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
    let transcript = checksum(&pool).await?;
    sqlx::raw_sql(
        "INSERT INTO rh_summary_access (summary_id, access_count, last_accessed_at)
         SELECT id, access_count * 2, '2024-01-01' FROM rh_summaries;
         UPDATE rh_wiki_extractions SET topics_touched = ARRAY['valve'];
         INSERT INTO rh_wiki_extractions (summary_id, status, pipeline_version, topics_touched, owner_user_id)
         SELECT id, 'failed', 8, ARRAY['gasket'], owner_user_id FROM rh_summaries
         WHERE id = '00000000-0000-0000-0000-00000000000b';",
    ).execute(&pool).await?;
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
    let access: (Uuid, i32) =
        sqlx::query_as("SELECT summary_id, access_count FROM rh_summary_access")
            .fetch_one(&pool)
            .await?;
    assert_eq!(access, (id(20), 10));
    let extraction: (Uuid, String, Vec<String>) =
        sqlx::query_as("SELECT summary_id, status, topics_touched FROM rh_wiki_extractions")
            .fetch_one(&pool)
            .await?;
    assert_eq!(
        extraction,
        (
            id(20),
            "done".to_owned(),
            vec!["gasket".to_owned(), "valve".to_owned()]
        )
    );
    assert_eq!(checksum(&pool).await?, transcript);
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
            at: Timestamp::now(),
        }])
        .await;
    assert!(
        matches!(fact, Err(FactError::SummaryReference(SummaryReferenceError::Gone(uuid))) if uuid == id(10))
    );

    let tags = store
        .store_tag_suggestions(TagSuggestionWrite {
            proposed_by: "fixture".to_owned(),
            at: Timestamp::now(),
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

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_references_split_message_has_only_the_first_anchor(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let store = scoped_store(&pool).await?;
    let mut prepared = tree(&[10, 11, 12], 20, &[2, 3], false);
    for (part, ordinal, messages) in [(21, 1, vec![(2, 1)]), (22, 2, vec![(2, 1), (3, 2)])] {
        prepared.records.push(PreparedSummaryRecord {
            id: id(part),
            parent: Some(id(20)),
            kind: SummaryKind::Part,
            ordinal,
            content: format!("Valve service part {ordinal}."),
            input_key: [5; 32],
            index_key: None,
            model: None,
            issued_part_ordinal: Some(ordinal),
            input_tokens: Some(20),
            quality_flags: 0,
            section: None,
            sources: messages
                .into_iter()
                .map(|(message, ordinal)| SummaryManifestEntry {
                    message: id(message),
                    ordinal,
                    render_key: None,
                })
                .collect(),
        });
    }
    assert_eq!(prepared.anchors().get(&id(2)), Some(&id(21)));
    prepared.part_order = vec![id(22), id(21)];
    assert_eq!(prepared.anchors().get(&id(2)), Some(&id(22)));
    prepared.part_order.clear();
    Scope::Maintenance
        .run(store.replace_summary_tree(&prepared))
        .await?;
    let found: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT message_id, anchor_id FROM rh_summary_deliveries WHERE consumer = 'entity' ORDER BY message_id",
    ).fetch_all(&pool).await?;
    assert_eq!(found, [(id(2), id(21)), (id(3), id(22))]);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_references_wiki_projects_committed_ids_after_retry(pool: PgPool) -> TestResult {
    use kernel::memory::MemoryStore;
    use kernel::wiki::WikiLookup;
    use kernel::wiki::page::{
        PageCitation, PageSource, WikiPage, WikiSlug, WikiSourceKind, parse_wiki_page,
    };
    use kernel::wiki::recompile::WikiRecompileStore;
    use memory_postgres::wiki_writer::WikiRepository;

    seed(&pool).await?;
    let before = checksum(&pool).await?;
    sqlx::query(
        "INSERT INTO rh_wiki_provenance (topic_slug, source_kind, source_id, owner_user_id)
         SELECT topic_slug, 'summary', summary_id, owner_user_id FROM rh_wiki_citations",
    )
    .execute(&pool)
    .await?;

    let directory = tempfile::tempdir()?;
    let slug = WikiSlug::try_from("valve".to_owned())?;
    let mut page = WikiPage::empty(slug.clone(), "Valve".to_owned());
    page.current_state = "The spare sits on the shelf.".to_owned();
    for summary in [id(10), id(12)] {
        page.citations.push(PageCitation {
            summary_id: summary.to_string(),
            date: None,
            kind: None,
            note: None,
        });
        page.meta.sources.push(PageSource {
            kind: WikiSourceKind::Summary,
            ids: vec![summary.to_string()],
            conversation_id: Some(id(1).to_string()),
            extra: wire::Extra::default(),
        });
    }
    async {
        let mut repository = WikiRepository::new(directory.path().to_path_buf())
            .lock()
            .await?;
        repository
            .fold(&mut page, &[], "Invented wiki page", Timestamp::now())
            .await?;
        repository.release().await
    }
    .await?;
    let page_path = directory.path().join("topics/valve.md");
    let cached = tokio::fs::read_to_string(&page_path).await?;
    assert!(cached.contains(&id(10).to_string()));
    let archive = directory.path().join("archive/valve");
    tokio::fs::create_dir_all(&archive).await?;
    let archival_file = archive.join("2024-01.1.md");
    tokio::fs::write(&archival_file, &cached).await?;

    let legacy_archive = directory.path().join("archive/valve.md");
    tokio::fs::write(
        &legacy_archive,
        format!(
            "## Citations\n- {}\n\n## Earlier notes\n\nThe spring is intact.\n",
            id(12)
        ),
    )
    .await?;

    let store = scoped_store(&pool)
        .await?
        .with_wiki_directory(directory.path().to_path_buf());
    sqlx::raw_sql(
        "CREATE FUNCTION refuse_swap() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN RAISE EXCEPTION 'interrupted publication'; END $$;
         CREATE CONSTRAINT TRIGGER refuse_swap AFTER DELETE ON rh_summaries
         DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION refuse_swap()",
    )
    .execute(&pool)
    .await?;
    let prepared = tree(&[10, 11, 12], 20, &[2], true);
    Scope::Maintenance
        .run(async {
            assert!(store.replace_summary_tree(&prepared).await.is_err());
            let WikiLookup::Page(first) = store.wiki_page(&slug).await? else {
                return Err("missing wiki page".into());
            };
            assert!(first.markdown.contains(&id(10).to_string()));

            sqlx::query("DROP TRIGGER refuse_swap ON rh_summaries")
                .execute(&pool)
                .await?;
            store.replace_summary_tree(&prepared).await?;
            let WikiLookup::Page(second) = store.wiki_page(&slug).await? else {
                return Err("missing wiki page".into());
            };
            let projected = parse_wiki_page(&second.markdown)?;
            assert_eq!(
                projected
                    .citations
                    .iter()
                    .map(|c| c.summary_id.as_str())
                    .collect::<Vec<_>>(),
                [id(20).to_string()]
            );
            assert_eq!(
                projected.meta.sources.first().ok_or("source")?.ids,
                [id(20).to_string()]
            );
            assert_eq!(projected.current_state, "The spare sits on the shelf.");
            let archive = store
                .wiki_archive(&slug, Some("2024-01"))
                .await?
                .ok_or("archive")?;
            let archived = parse_wiki_page(&archive)?;
            assert_eq!(archived.citations, []);
            assert_eq!(archived.meta.sources, []);
            assert_eq!(archived.current_state, projected.current_state);
            let legacy = store
                .wiki_archive(&slug, None)
                .await?
                .ok_or("legacy archive")?;
            assert!(legacy.contains("The spring is intact."));
            assert!(!legacy.contains(&id(12).to_string()));
            let recompile = store
                .recompile_source(&slug, Timestamp::now())
                .await?
                .ok_or("recompile")?;
            assert_eq!(recompile.page.citations, []);
            assert_eq!(recompile.page.meta.sources, []);
            assert_eq!(
                recompile
                    .summaries
                    .first()
                    .and_then(|s| s.input.as_ref())
                    .map(|s| s.id),
                Some(id(20))
            );
            Ok::<_, Box<dyn std::error::Error>>(())
        })
        .await?;
    assert_eq!(tokio::fs::read_to_string(page_path).await?, cached);
    assert_eq!(tokio::fs::read_to_string(archival_file).await?, cached);
    assert_eq!(checksum(&pool).await?, before);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_references_reopens_parts_and_sections(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let store = scoped_store(&pool).await?;
    Scope::Maintenance
        .run(store.replace_summary_tree(&initial_parts()))
        .await?;
    sqlx::query(
        "INSERT INTO rh_messages (id, conversation_id, agent, channel, role, content, created_at, owner_user_id)
         VALUES ($1, $2, 'fixture', 'cli', 'user', 'An earlier measurement.', '2023-12-01', 'alice')",
    ).bind(id(4)).bind(id(1)).execute(&pool).await?;
    sqlx::query(
        "INSERT INTO rh_wiki_citations (topic_slug, summary_id, kind, note, owner_user_id)
         VALUES ('valve', $1, 'part', 'A kept part', 'alice'),
                ('valve', $2, 'part', 'Another kept part', 'alice')
         ON CONFLICT DO NOTHING",
    )
    .bind(id(21))
    .bind(id(22))
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO rh_summary_passages (id, record_id, ordinal, char_start, char_end,
            content, earliest_at, latest_at, index_key, embedding)
         SELECT gen_random_uuid(), id, 0, 0, length(content), content, earliest_at, latest_at,
                input_key, array_fill(0.25::real, ARRAY[1024])::halfvec
         FROM rh_summaries WHERE kind = 'part'",
    )
    .execute(&pool)
    .await?;
    let transcript = checksum(&pool).await?;
    let parts = || {
        sqlx::query_scalar::<_, String>(
        "SELECT md5(jsonb_build_array(
            (SELECT jsonb_agg(to_jsonb(s) - 'parent_id' ORDER BY id) FROM rh_summaries s WHERE id IN ($1, $2)),
            (SELECT jsonb_agg(to_jsonb(l) ORDER BY summary_id, ordinal) FROM rh_summary_sources l WHERE summary_id IN ($1, $2)),
            (SELECT jsonb_agg(to_jsonb(c) ORDER BY summary_id) FROM rh_wiki_citations c WHERE summary_id IN ($1, $2)),
            (SELECT jsonb_agg(to_jsonb(d) ORDER BY consumer, message_id) FROM rh_summary_deliveries d WHERE anchor_id IN ($1, $2)),
            (SELECT jsonb_agg(to_jsonb(p) ORDER BY record_id, ordinal) FROM rh_summary_passages p WHERE record_id IN ($1, $2))
         )::text)",
    ).bind(id(21)).bind(id(22)).fetch_one(&pool)
    };
    let plan = reopen_parts();
    let update_key = |byte: u8| {
        sqlx::query("UPDATE rh_summary_sources SET render_key = $2 WHERE summary_id = $1")
            .bind(id(21))
            .bind(vec![byte; 32])
            .execute(&pool)
    };
    update_key(3).await?;
    let changed = parts().await?;
    assert!(matches!(
        Scope::Maintenance.run(store.replace_summary_tree(&plan)).await,
        Err(memory_postgres::ReplaceSummaryError::Reference(SummaryReferenceError::Stale(uuid))) if uuid == id(21)
    ));
    assert_eq!(parts().await?, changed);
    update_key(9).await?;
    let before = parts().await?;
    assert_eq!(checksum(&pool).await?, transcript);
    Scope::Maintenance
        .run(store.replace_summary_tree(&reopen_parts()))
        .await?;
    assert_eq!(parts().await?, before);
    assert_eq!(checksum(&pool).await?, transcript);

    let graph: Vec<(Uuid, Option<Uuid>)> =
        sqlx::query_as("SELECT id, parent_id FROM rh_summaries ORDER BY id")
            .fetch_all(&pool)
            .await?;
    assert_eq!(
        graph,
        [
            (id(21), Some(id(30))),
            (id(22), Some(id(30))),
            (id(23), Some(id(30))),
            (id(30), None),
            (id(31), Some(id(30))),
            (id(33), Some(id(30))),
        ]
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_references_a_running_wiki_job_cannot_apply_or_retry(pool: PgPool) -> TestResult {
    use kernel::queue::{ClaimSelector, FixedJob, JobName, JobQueue};
    use memory_postgres::PostgresQueue;

    seed(&pool).await?;
    let before = checksum(&pool).await?;
    let store = scoped_store(&pool).await?;
    let queue = PostgresQueue::connect(pool.clone()).await?;
    let job = ExtractWiki {
        summary_id: id(11),
        conversation_id: Some(id(1)),
    };
    sqlx::query(
        "SELECT graphile_worker.add_job('extract-wiki', json_build_object('summaryId', $1::text),
            job_key => 'wiki-ext-' || $1::text)",
    )
    .bind(job.summary_id)
    .execute(&pool)
    .await?;
    let selector = ClaimSelector::matching(vec![JobName::Fixed(FixedJob::ExtractWiki)]);
    let claimed = queue.claim(&selector).await?.ok_or("expected a claim")?;
    Scope::Maintenance
        .run(async {
            let WikiWork::Pending(input) = store.wiki_work(&job).await? else {
                return Err("expected running wiki work".into());
            };
            store
                .replace_summary_tree(&tree(&[10, 11, 12], 20, &[2], true))
                .await?;
            let result = store
                .apply_wiki(
                    &job,
                    Vec::new(),
                    &WikiRouting {
                        input: Some(input),
                        ..WikiRouting::default()
                    },
                    Timestamp::now(),
                )
                .await;
            assert!(matches!(result, Err(error) if error.obsolete_input()));
            queue.release(claimed.id).await?;
            assert!(queue.read(claimed.id).await?.is_none());
            assert!(queue.claim(&selector).await?.is_none());
            Ok::<_, Box<dyn std::error::Error>>(())
        })
        .await?;
    assert_eq!(checksum(&pool).await?, before);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_wiki_extractions")
        .fetch_one(&pool)
        .await?;
    assert_eq!(rows, 1);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_references_reuses_passage_vectors_by_window_and_spec(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let transcript = checksum(&pool).await?;
    let store = scoped_store(&pool)
        .await?
        .with_summary_index_key([0xab; 32]);
    let mut first = tree(&[10, 11, 12], 20, &[2, 3], true);
    let first_root = first.records.first_mut().ok_or("missing first root")?;
    first_root.content = "Screw. Clamp.".to_owned();
    first_root.index_key = Some([0xab; 32]);
    first.passage_windows.insert(id(20), vec![0..7, 7..13]);
    Scope::Maintenance
        .run(store.replace_summary_tree(&first))
        .await?;
    sqlx::raw_sql(
        "UPDATE rh_summary_passages SET embedding = array_fill((ordinal + 1)::real, ARRAY[1024])::halfvec;
         UPDATE rh_summaries SET index_needs_repair = false",
    ).execute(&pool).await?;
    let passages = || {
        sqlx::query_as::<_, (Uuid, String, bool)>(
            "SELECT id, content, embedding IS NOT NULL FROM rh_summary_passages ORDER BY ordinal",
        )
        .fetch_all(&pool)
    };
    let before = passages().await?;
    assert_eq!(before.len(), 2);
    let mut second = tree(&[20], 21, &[2, 3], true);
    let second_root = second.records.first_mut().ok_or("missing second root")?;
    second_root.content = "Screw. Clamp. Nut.".to_owned();
    second_root.index_key = Some([0xab; 32]);
    second
        .passage_windows
        .insert(id(21), vec![0..7, 7..13, 13..18]);
    Scope::Maintenance
        .run(store.replace_summary_tree(&second))
        .await?;
    let after = passages().await?;
    assert_eq!(after.get(..2), Some(before.as_slice()));
    assert_eq!(
        after
            .last()
            .map(|(_, text, indexed)| (text.as_str(), *indexed)),
        Some((" Nut.", false))
    );

    let mut different = tree(&[21], 22, &[2, 3], true);
    let previous_root = second.records.first().ok_or("missing second root")?;
    let different_root = different
        .records
        .first_mut()
        .ok_or("missing different root")?;
    different_root.content.clone_from(&previous_root.content);
    different_root.index_key = Some([0xcd; 32]);
    different
        .passage_windows
        .insert(id(22), vec![0..7, 7..13, 13..18]);
    let changed_store = store.with_summary_index_key([0xcd; 32]);
    Scope::Maintenance
        .run(changed_store.replace_summary_tree(&different))
        .await?;
    assert!(passages().await?.iter().all(|(candidate, _, embedded)| {
        !embedded && after.iter().all(|(old, _, _)| old != candidate)
    }));
    assert_eq!(checksum(&pool).await?, transcript);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_swap_keeps_the_old_session_visible_until_commit(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let store = scoped_store(&pool).await?;
    Scope::Maintenance
        .run(store.replace_summary_tree(&tree(&[10, 11, 12], 20, &[2, 3], true)))
        .await?;
    let mut blocked = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(85112)")
        .execute(&mut *blocked)
        .await?;
    sqlx::raw_sql(
        "CREATE FUNCTION test_wait_for_publication() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN PERFORM pg_advisory_xact_lock(85112); RETURN NEW; END
         $$;
         CREATE TRIGGER test_wait_for_publication BEFORE INSERT ON rh_summaries
         FOR EACH ROW WHEN (NEW.kind = 'session') EXECUTE FUNCTION test_wait_for_publication();",
    )
    .execute(&pool)
    .await?;
    let prepared = tree(&[20], 21, &[2, 3], true);
    let replacement = Scope::Maintenance.run(store.replace_summary_tree(&prepared));
    let reader = async {
        await_publication_lock(&pool).await?;
        assert_eq!(visible_sessions(&pool).await?, [id(20)]);
        blocked.commit().await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    tokio::try_join!(async { replacement.await.map_err(Into::into) }, reader)?;
    assert_eq!(visible_sessions(&pool).await?, [id(21)]);
    Ok(())
}

async fn await_publication_lock(pool: &PgPool) -> TestResult {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE locktype = 'advisory'
                                AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
                                AND objid = 85112 AND NOT granted)",
            )
            .fetch_one(pool)
            .await?;
            if waiting {
                return Ok::<(), sqlx::Error>(());
            }
        }
    })
    .await??;
    Ok(())
}

async fn visible_sessions(pool: &PgPool) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar("SELECT id FROM rh_summaries WHERE kind = 'session'")
        .fetch_all(pool)
        .await
}
