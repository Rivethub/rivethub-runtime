use jiff::Timestamp;
use kernel::memory::summary_reference::{
    PreparedSummaryRecord, PreparedSummaryTree, SummaryManifestEntry, SummaryReferenceError,
    SummarySectionBounds, embedding_input_hash,
};
use kernel::memory::{
    EmbedTable, EmbedTarget, EmbeddingStore, EmbeddingStoreError, StoredEmbedding, SummaryKind,
};
use kernel::wiki::builder::{ExtractWiki, WikiBuilderStore, WikiRouting, WikiStoreError, WikiWork};
use memory_sqlite::{SqliteError, SqliteStore};
use rusqlite::params;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

use crate::search_support::{TestResult, failed, store};

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

async fn seed(store: &SqliteStore) -> TestResult {
    store.with_conn(|conn| {
        let at = "2024-01-01T00:00:00Z";
        conn.execute(
            "INSERT INTO ros_conversations (id, session_key, agent, channel, created_at, updated_at, owner_user_id)
             VALUES (?1, 'valves', 'fixture', 'cli', ?2, ?2, 'alice')",
            params![id(1).to_string(), at],
        ).map_err(failed)?;
        for (message, content) in [(2, "Seal the brass valve."), (3, "Check the spare gasket.")] {
            conn.execute(
                "INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, created_at)
                 VALUES (?1, ?2, 'fixture', 'cli', 'user', ?3, ?4)",
                params![id(message).to_string(), id(1).to_string(), content, at],
            ).map_err(failed)?;
        }
        conn.execute(
            "INSERT INTO ros_wiki_topics (slug, title, created_at, updated_at) VALUES ('valve', 'Valve', ?1, ?1)",
            [at],
        ).map_err(failed)?;
        for (summary, message, accesses, note) in [(10, 2, 2, "first"), (11, 2, 3, "latest"), (12, 3, 9, "unmapped")] {
            let summary = id(summary).to_string();
            conn.execute(
                "INSERT INTO ros_summaries (id, conversation_id, content, access_count, created_at)
                 VALUES (?1, ?2, 'Earlier summary.', ?3, ?4)",
                params![summary, id(1).to_string(), accesses, at],
            ).map_err(failed)?;
            conn.execute("INSERT INTO ros_summary_sources (summary_id, message_id, ordinal) VALUES (?1, ?2, 0)",
                params![summary, id(message).to_string()]).map_err(failed)?;
            conn.execute(
                "INSERT INTO ros_wiki_citations (topic_slug, summary_id, kind, note, cited_at)
                 VALUES ('valve', ?1, 'leaf', ?2, ?3)",
                params![summary, note, at],
            ).map_err(failed)?;
        }
        conn.execute(
            "INSERT INTO ros_wiki_extractions (summary_id, status, pipeline_version, extracted_at)
             VALUES (?1, 'done', 4, ?2)",
            params![id(10).to_string(), at],
        ).map_err(failed)?;
        Ok(())
    }).await?;
    Ok(())
}

async fn snapshot(store: &SqliteStore) -> Result<String, memory_sqlite::SqliteError> {
    use sha2::{Digest as _, Sha256};

    store
        .with_conn(|conn| {
            let mut query = conn
                .prepare("SELECT * FROM ros_messages ORDER BY id")
                .map_err(failed)?;
            let width = query.column_count();
            let mut rows = query.query([]).map_err(failed)?;
            let mut digest = Sha256::new();
            while let Some(row) = rows.next().map_err(failed)? {
                let values = (0..width)
                    .map(|column| row.get::<_, rusqlite::types::Value>(column))
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(failed)?;
                digest.update(format!("{values:?}").as_bytes());
            }
            Ok(hex::encode(digest.finalize()))
        })
        .await
}

#[tokio::test]
async fn top_down_references_swap_keeps_receipts_and_remaps_or_deletes_references() -> TestResult {
    let (_dir, store) = store(None)?;
    seed(&store).await?;
    let before = snapshot(&store).await?;
    store
        .replace_summary_tree(tree(&[10, 11, 12], 20, &[2], true))
        .await?;
    store.with_conn(|conn| {
        let citation: (String, String, String) = conn.query_row(
            "SELECT summary_id, kind, note FROM ros_wiki_citations", [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).map_err(failed)?;
        assert_eq!(citation, (id(20).to_string(), "session".to_owned(), "latest".to_owned()));
        let accesses: i32 = conn.query_row("SELECT access_count FROM ros_summaries", [], |row| row.get(0)).map_err(failed)?;
        assert_eq!(accesses, 5);
        let receipts: String = conn.query_row(
            "SELECT group_concat(coalesce(completed_version, -1) || ':' || historical_backlog)
             FROM (SELECT * FROM ros_summary_deliveries WHERE consumer = 'wiki' ORDER BY message_id)",
            [], |row| row.get(0),
        ).map_err(failed)?;
        assert_eq!(receipts, "4:0,-1:1");
        Ok(())
    }).await?;
    assert_eq!(snapshot(&store).await?, before);
    let gone = store
        .apply_wiki(
            &ExtractWiki {
                summary_id: id(10),
                conversation_id: Some(id(1)),
            },
            Vec::new(),
            &WikiRouting::default(),
            Timestamp::now(),
        )
        .await;
    assert!(
        matches!(gone, Err(WikiStoreError::Reference(SummaryReferenceError::Gone(uuid))) if uuid == id(10))
    );
    let embed = store
        .write(StoredEmbedding {
            target: EmbedTarget {
                table: EmbedTable::Summaries,
                id: id(10).to_string(),
            },
            vector: vec![1.; 1024],
            content_hash: embedding_input_hash("Earlier summary."),
            chunks: Vec::new(),
        })
        .await;
    assert!(
        matches!(embed, Err(EmbeddingStoreError::SummaryReference(SummaryReferenceError::Gone(uuid))) if uuid == id(10))
    );
    check_gone(&store).await?;
    Ok(())
}

#[tokio::test]
async fn top_down_references_wiki_acceptance_and_receipts_commit_together() -> TestResult {
    let (_dir, store) = store(None)?;
    seed(&store).await?;
    store
        .replace_summary_tree(tree(&[10, 11, 12], 20, &[2, 3], false))
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
        .replace_summary_tree(tree(&[20], 21, &[2, 3], false))
        .await?;
    let work = store
        .wiki_work(&ExtractWiki {
            summary_id: id(21),
            ..job
        })
        .await?;
    assert!(matches!(work, WikiWork::Complete { .. }));
    Ok(())
}

#[tokio::test]
async fn top_down_references_related_rows_follow_the_winner() -> TestResult {
    let (_dir, store) = store(None)?;
    seed(&store).await?;
    let transcript = snapshot(&store).await?;
    store.with_conn(seed_related_references).await?;
    store
        .replace_summary_tree(tree(&[10, 11, 12], 20, &[2], true))
        .await?;
    store
        .with_conn(|conn| {
            for statement in [
                "SELECT source_id FROM ros_wiki_provenance",
                "SELECT summary_id FROM rh_wiki_entity_candidates",
                "SELECT summary_id FROM rh_wiki_deferred",
                "SELECT entity_id FROM ros_tags WHERE entity_type = 'summary'",
                "SELECT summary_id FROM rh_entity_fact_claims",
            ] {
                let found: Vec<String> = conn
                    .prepare(statement)
                    .and_then(|mut stmt| stmt.query_map([], |row| row.get(0))?.collect())
                    .map_err(failed)?;
                assert_eq!(found, [id(20).to_string()], "{statement}");
            }
            let jobs: i64 = conn
                .query_row(
                    "SELECT count(*) FROM ros_jobs WHERE job_key LIKE 'wiki-ext-%'",
                    [],
                    |row| row.get(0),
                )
                .map_err(failed)?;
            assert_eq!(jobs, 0);
            let extraction: (String, String, String) = conn
                .query_row(
                    "SELECT summary_id, status, topics_touched FROM ros_wiki_extractions",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .map_err(failed)?;
            assert_eq!(
                extraction,
                (
                    id(20).to_string(),
                    "done".to_owned(),
                    r#"["gasket","valve"]"#.to_owned()
                )
            );
            Ok(())
        })
        .await?;
    assert_eq!(snapshot(&store).await?, transcript);
    Ok(())
}

fn seed_related_references(conn: &mut rusqlite::Connection) -> Result<(), SqliteError> {
    let at = "2024-01-01T00:00:00Z";
    conn.execute(
        "UPDATE ros_wiki_extractions SET topics_touched = '[\"valve\"]'",
        [],
    )
    .map_err(failed)?;
    conn.execute(
            "INSERT INTO ros_wiki_extractions (summary_id, status, pipeline_version, extracted_at, topics_touched)
             VALUES (?1, 'failed', 8, ?2, '[\"gasket\"]')",
            params![id(11).to_string(), at],
        ).map_err(failed)?;
    for statement in [
            "INSERT INTO ros_wiki_provenance (topic_slug, source_kind, source_id, created_at)
             SELECT 'valve', 'summary', id, ?1 FROM ros_summaries",
            "INSERT INTO rh_wiki_entity_candidates (key, source, summary_id, patch, held_at, owner_user_id)
             SELECT 'valve', 'summary:' || id, id, '{}', ?1, 'alice' FROM ros_summaries",
            "INSERT INTO rh_wiki_deferred (summary_id, waits_conversation, patches, deferred_at, owner_user_id)
             SELECT id, conversation_id, '[]', ?1, 'alice' FROM ros_summaries",
            "INSERT INTO ros_tags (id, entity_type, entity_id, key, value, source, state, created_at, updated_at)
             SELECT id, 'summary', id, 'type', 'hardware', 'user', 'accepted', ?1, ?1 FROM ros_summaries",
            "INSERT INTO rh_entity_facts (id, topic_id, key, created_at)
             SELECT 'valve-size', id, 'size', ?1 FROM rh_topics WHERE slug = 'hardware' AND kind = 'type'",
            "INSERT INTO rh_entity_fact_claims (id, fact_id, value, actor, session_id, summary_id, at, recorded_at, owner_user_id)
             SELECT id, 'valve-size', id, 'wiki-writer', 'valves', id, ?1, ?1, 'alice' FROM ros_summaries",
        ] {
            conn.execute(statement, [at]).map_err(failed)?;
        }
    conn.execute(
            "INSERT INTO ros_jobs (id, task, job_key, payload, run_at, created_at, updated_at)
             VALUES ('wiki-fixture', 'extract-wiki', 'wiki-ext-' || ?1, json_object('summaryId', ?1), ?2, ?2, ?2)",
            params![id(12).to_string(), at],
        ).map_err(failed)?;
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

#[tokio::test]
async fn top_down_references_split_message_has_only_the_first_anchor() -> TestResult {
    let (_dir, store) = store(None)?;
    seed(&store).await?;
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
    store.replace_summary_tree(prepared).await?;
    store.with_conn(|conn| {
        let mut query = conn.prepare(
            "SELECT message_id, anchor_id FROM ros_summary_deliveries WHERE consumer = 'entity' ORDER BY message_id",
        ).map_err(failed)?;
        let found: Vec<(String, String)> = query.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(failed)?.collect::<Result<_, _>>().map_err(failed)?;
        assert_eq!(found, [(id(2).to_string(), id(21).to_string()), (id(3).to_string(), id(22).to_string())]);
        Ok(())
    }).await?;
    Ok(())
}

async fn kept_parts(store: &SqliteStore) -> Result<String, memory_sqlite::SqliteError> {
    use sha2::{Digest as _, Sha256};

    store.with_conn(|conn| {
        let mut digest = Sha256::new();
        for statement in [
            "SELECT * FROM ros_summaries WHERE id IN (?1, ?2) ORDER BY id",
            "SELECT * FROM ros_summary_sources WHERE summary_id IN (?1, ?2) ORDER BY summary_id, ordinal",
            "SELECT * FROM ros_wiki_citations WHERE summary_id IN (?1, ?2) ORDER BY summary_id",
            "SELECT * FROM ros_summary_deliveries WHERE anchor_id IN (?1, ?2) ORDER BY consumer, message_id",
        ] {
            let mut stmt = conn.prepare(statement).map_err(failed)?;
            let columns: Vec<_> = stmt.column_names().enumerate()
                .filter_map(|(i, name)| (name != "parent_id").then_some(i)).collect();
            let mut rows = stmt.query([id(21).to_string(), id(22).to_string()]).map_err(failed)?;
            while let Some(row) = rows.next().map_err(failed)? {
                for &column in &columns {
                    let value: rusqlite::types::Value = row.get(column).map_err(failed)?;
                    digest.update(format!("{value:?}").as_bytes());
                }
            }
        }
        Ok(hex::encode(digest.finalize()))
    }).await
}

#[tokio::test]
async fn top_down_references_reopens_parts_and_sections() -> TestResult {
    let (_dir, store) = store(None)?;
    seed(&store).await?;
    store.replace_summary_tree(initial_parts()).await?;
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, created_at)
             VALUES (?1, ?2, 'fixture', 'cli', 'user', 'An earlier measurement.', '2023-12-01T00:00:00Z')",
            [id(4).to_string(), id(1).to_string()],
        ).map_err(failed)?;
        conn.execute("UPDATE ros_summaries SET embedding = X'0000803F00000000',
                          index_key = input_key, index_needs_repair = 0 WHERE kind = 'part'", [])
            .map_err(failed)?;
        for part in [id(21), id(22)] {
            conn.execute(
                "INSERT INTO ros_wiki_citations (topic_slug, summary_id, kind, note, cited_at)
                 VALUES ('valve', ?1, 'part', 'A kept part', '2024-01-01T00:00:00Z')
                 ON CONFLICT DO NOTHING",
                [part.to_string()],
            ).map_err(failed)?;
        }
        Ok(())
    }).await?;

    let transcript = snapshot(&store).await?;
    let plan = reopen_parts();
    let update_key = |byte: u8| {
        store.with_conn(move |conn| {
            conn.execute(
                "UPDATE ros_summary_sources SET render_key = ?2 WHERE summary_id = ?1",
                params![id(21).to_string(), vec![byte; 32]],
            )
            .map_err(failed)
        })
    };
    update_key(3).await?;
    let changed = kept_parts(&store).await?;
    assert!(matches!(
        store.replace_summary_tree(plan).await,
        Err(memory_sqlite::SqliteError::SummaryReference(SummaryReferenceError::Stale(uuid))) if uuid == id(21)
    ));
    assert_eq!(kept_parts(&store).await?, changed);
    update_key(9).await?;
    let before = kept_parts(&store).await?;
    assert_eq!(snapshot(&store).await?, transcript);
    store.replace_summary_tree(reopen_parts()).await?;
    assert_eq!(kept_parts(&store).await?, before);
    assert_eq!(snapshot(&store).await?, transcript);
    let graph = store
        .with_conn(|conn| {
            conn.prepare("SELECT id, parent_id FROM ros_summaries ORDER BY id")
                .and_then(|mut stmt| {
                    stmt.query_map([], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()
                })
                .map_err(failed)
        })
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
        .map(|(node, parent)| (node.to_string(), parent.map(|p| p.to_string())))
    );
    Ok(())
}

#[tokio::test]
async fn top_down_references_writer_and_replacement_contend_across_connections() -> TestResult {
    use std::time::Duration;
    use tokio::sync::oneshot;

    let (dir, store) = store(None)?;
    seed(&store).await?;
    let writer = SqliteStore::open(dir.path().join("memory.sqlite"))?;
    let before = snapshot(&store).await?;
    let (locked, held) = oneshot::channel();
    let (unlock, release) = oneshot::channel();
    let writing = tokio::spawn(async move {
        writer
            .with_conn(move |conn| {
                let tx = conn
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .map_err(failed)?;
                tx.execute(
                    "INSERT INTO ros_wiki_topics (slug, title, created_at, updated_at)
                 VALUES ('spare', 'Spare', '2024-01-02', '2024-01-02')",
                    [],
                )
                .map_err(failed)?;
                tx.execute(
                    "INSERT INTO ros_wiki_citations (topic_slug, summary_id, kind, note, cited_at)
                 VALUES ('spare', ?1, 'leaf', 'Written before the swap', '2024-01-02')",
                    [id(11).to_string()],
                )
                .map_err(failed)?;
                locked
                    .send(())
                    .map_err(|()| failed(rusqlite::Error::InvalidQuery))?;
                release
                    .blocking_recv()
                    .map_err(|_| failed(rusqlite::Error::InvalidQuery))?;
                tx.commit().map_err(failed)
            })
            .await
    });
    held.await?;
    let replacing = store.replace_summary_tree(tree(&[10, 11, 12], 20, &[2], true));
    tokio::pin!(replacing);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut replacing)
            .await
            .is_err()
    );
    unlock.send(()).map_err(|()| "writer stopped")?;
    writing.await??;
    tokio::time::timeout(Duration::from_secs(5), replacing).await??;
    let cited = store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT summary_id FROM ros_wiki_citations WHERE topic_slug = 'spare'",
                [],
                |row| row.get::<_, String>(0),
            )
            .map_err(failed)
        })
        .await?;
    assert_eq!(cited, id(20).to_string());
    assert_eq!(snapshot(&store).await?, before);
    Ok(())
}

#[tokio::test]
async fn top_down_references_a_running_wiki_job_cannot_apply_or_retry() -> TestResult {
    use kernel::queue::{ClaimSelector, FixedJob, JobName, JobQueue};
    use memory_sqlite::JobInsert;

    let (_dir, store) = store(None)?;
    seed(&store).await?;
    let before = snapshot(&store).await?;
    let now = Timestamp::now();
    let job = ExtractWiki {
        summary_id: id(11),
        conversation_id: Some(id(1)),
    };
    let payload = serde_json::to_string(&job)?;
    store
        .enqueue_job(
            JobInsert::new("extract-wiki", payload, now)
                .key(format!("wiki-ext-{}", job.summary_id)),
        )
        .await?;
    let selector = ClaimSelector::matching(vec![JobName::Fixed(FixedJob::ExtractWiki)]);
    let claimed = store.claim(&selector).await?.ok_or("expected a claim")?;
    let WikiWork::Pending(input) = store.wiki_work(&job).await? else {
        return Err("expected running wiki work".into());
    };
    store
        .replace_summary_tree(tree(&[10, 11, 12], 20, &[2], true))
        .await?;
    let result = store
        .apply_wiki(
            &job,
            Vec::new(),
            &WikiRouting {
                input: Some(input),
                ..WikiRouting::default()
            },
            now,
        )
        .await;
    assert!(matches!(result, Err(error) if error.obsolete_input()));
    store.release(claimed.id).await?;
    assert!(store.read(claimed.id).await?.is_none());
    assert!(store.claim(&selector).await?.is_none());
    assert_eq!(snapshot(&store).await?, before);
    Ok(())
}

#[tokio::test]
async fn top_down_references_kept_part_competes_with_incoming_rows() -> TestResult {
    let (_dir, store) = store(None)?;
    seed(&store).await?;
    store.replace_summary_tree(initial_parts()).await?;
    store.with_conn(seed_kept_collision).await?;
    let before = snapshot(&store).await?;
    let mut prepared = reopen_parts();
    prepared
        .records
        .retain(|row| row.kind != SummaryKind::Section);
    store.replace_summary_tree(prepared.clone()).await?;
    assert!(store.replace_summary_tree(prepared).await.is_err());
    assert_eq!(snapshot(&store).await?, before);
    let actual: (String, String, i32, i32, bool, String) = store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT c.note, e.status, e.pipeline_version, a.access_count,
                    a.last_accessed_at = '2025-06-01T00:00:00Z', e.topics_touched
             FROM ros_wiki_citations c JOIN ros_wiki_extractions e USING (summary_id)
             JOIN ros_summaries a ON a.id = c.summary_id WHERE c.summary_id = ?1",
                [id(21).to_string()],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .map_err(failed)
        })
        .await?;
    assert_eq!(
        actual,
        (
            "new note".to_owned(),
            "done".to_owned(),
            4,
            11,
            true,
            r#"["gasket","valve"]"#.to_owned(),
        )
    );
    let winners: (String, String, String) = store.with_conn(|conn| conn.query_row(
        "SELECT t.id, c.id, json_extract(d.patches, '$[0].slug') || ',' || json_extract(d.patches, '$[1].slug')
         FROM ros_tags t JOIN rh_entity_fact_claims c ON c.summary_id = t.entity_id
         JOIN rh_wiki_deferred d ON d.summary_id = t.entity_id WHERE t.entity_id = ?1",
        [id(21).to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).map_err(failed)).await?;
    assert_eq!(
        winners,
        (
            id(23).to_string(),
            id(23).to_string(),
            "gasket,valve".to_owned()
        )
    );
    Ok(())
}

fn seed_kept_collision(conn: &mut rusqlite::Connection) -> Result<(), SqliteError> {
    conn.execute(
        "INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, created_at)
         VALUES (?1, ?2, 'fixture', 'cli', 'user', 'A new measurement.', '2023-12-01T00:00:00Z')",
        params![id(4).to_string(), id(1).to_string()],
    )
    .map_err(failed)?;
    conn.execute(
        "INSERT INTO rh_entity_facts (id, topic_id, key, created_at)
         SELECT 'diameter', id, 'diameter', '2020-01-01' FROM rh_topics WHERE slug = 'hardware' AND kind = 'type'",
        [],
    ).map_err(failed)?;
    for sql in [
        "INSERT INTO ros_wiki_citations (topic_slug, summary_id, kind, note, cited_at)
         VALUES ('valve', ?1, 'part', 'old note', '2020-01-01T00:00:00Z'),
                ('valve', ?2, 'section', 'new note', '2025-06-01T00:00:00Z')",
        r#"INSERT INTO ros_wiki_extractions (summary_id, status, pipeline_version, topics_touched, extracted_at)
         VALUES (?1, 'failed', 1, '["gasket"]', '2020-01-01T00:00:00Z'),
                (?2, 'done', 4, '["valve"]', '2025-06-01T00:00:00Z')"#,
        "UPDATE ros_summaries SET access_count = CASE WHEN id = ?1 THEN 2 ELSE 9 END,
             last_accessed_at = CASE WHEN id = ?1 THEN '2020-01-01T00:00:00Z' ELSE '2025-06-01T00:00:00Z' END
         WHERE id IN (?1, ?2)",
        "INSERT INTO ros_tags (id, entity_type, entity_id, key, value, source, state, created_at, updated_at)
         VALUES (?1, 'summary', ?1, 'type', 'hardware', 'user', 'accepted', '2020-01-01', '2020-01-01'),
                (?2, 'summary', ?2, 'type', 'hardware', 'user', 'rejected', '2025-06-01', '2025-06-01')",
        "INSERT INTO rh_entity_fact_claims
            (id, fact_id, value, actor, session_id, summary_id, at, recorded_at, owner_user_id)
         VALUES (?1, 'diameter', 'small', 'wiki-writer', 'valves', ?1, '2020-01-01', '2020-01-01', 'alice'),
                (?2, 'diameter', 'large', 'wiki-writer', 'valves', ?2, '2025-06-01', '2025-06-01', 'alice')",
        r#"INSERT INTO rh_wiki_deferred (summary_id, waits_conversation, patches, deferred_at, owner_user_id)
         VALUES (?1, '00000000-0000-0000-0000-000000000001', '[{"slug":"valve"}]', '2020-01-01', 'alice'),
                (?2, '00000000-0000-0000-0000-000000000001', '[{"slug":"gasket"}]', '2025-06-01', 'alice')"#,
    ] {
        conn.execute(sql, params![id(21).to_string(), id(23).to_string()]).map_err(failed)?;
    }
    Ok(())
}
