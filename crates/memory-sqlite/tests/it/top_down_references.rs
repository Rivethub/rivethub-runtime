use chrono::Utc;
use kernel::memory::summary_reference::{
    PreparedSummaryRecord, PreparedSummaryTree, SummaryManifestEntry, SummaryReferenceError,
    embedding_input_hash,
};
use kernel::memory::{
    EmbedTable, EmbedTarget, EmbeddingStore, EmbeddingStoreError, StoredEmbedding, SummaryKind,
};
use kernel::wiki::builder::{ExtractWiki, WikiBuilderStore, WikiRouting, WikiStoreError, WikiWork};
use memory_sqlite::SqliteStore;
use rusqlite::params;
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
    store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT json_group_array(json_array(id, conversation_id, content, created_at)) FROM
         (SELECT * FROM ros_messages ORDER BY id)",
                [],
                |row| row.get(0),
            )
            .map_err(failed)
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
            Utc::now(),
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
            Utc::now(),
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
    store.with_conn(|conn| {
        let at = "2024-01-01T00:00:00Z";
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
    }).await?;
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
            Ok(())
        })
        .await?;
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
