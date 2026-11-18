use crate::search_support;
use jiff::Timestamp;

use kernel::memory::SearchMode;
use kernel::memory::{BrowseWindow, MemoryScope};
use kernel::model::Embedding;
use memory_sqlite::SqliteError;
use search_support::{TestResult, failed, query, store};
use std::sync::Arc;
use testkit::fakes::{FakeProvider, Script};

async fn seeded(
    provider: Option<Arc<FakeProvider>>,
) -> TestResult<(tempfile::TempDir, memory_sqlite::SqliteStore)> {
    let (dir, store) = store(provider)?;
    store.with_conn(|conn| {
        conn.execute_batch("INSERT INTO ros_conversations(id,session_key,agent,channel,created_at,updated_at) VALUES ('c','s','agent','cli','2026-10-10T00:00:00.000Z','2026-10-10T00:00:00.000Z');
            INSERT INTO ros_messages(id,conversation_id,role,content,agent,channel,created_at,embedding) VALUES
            ('a','c','user','alpha search content with enough words to enter the lexical arm','agent','cli','2026-10-10T00:00:00.000Z',X'0000803F00000000'),
            ('b','c','assistant','semantically related passage without the keyword','agent','cli','2026-10-10T00:00:00.000Z',X'000000000000803F');
            INSERT INTO ros_summaries(id,conversation_id,content,kind,created_at,embedding) VALUES
            ('s','c','alpha search summary with enough words to enter the lexical arm','leaf','2026-10-10T00:00:00.000Z',X'0000803F00000000');").map_err(failed)?;
        Ok(())
    }).await?;
    Ok((dir, store))
}

fn provider() -> Arc<FakeProvider> {
    Arc::new(FakeProvider::new(vec![Script::Embed(vec![Embedding {
        vector: vec![2.0, 0.0],
    }])]))
}

#[tokio::test]
async fn vector_scores_messages_and_summaries_and_normalizes_query() -> TestResult {
    let (_dir, store) = seeded(Some(provider())).await?;
    let now = wire::time::timestamp_at_utc(2026, 10, 10, 0, 0, 0).unwrap();
    let response = store
        .search_at(
            query("unmatched", SearchMode::Vector),
            now,
            &jiff::tz::TimeZone::UTC,
        )
        .await?;
    assert!(response.degraded.is_none());
    assert_eq!(
        response
            .results
            .iter()
            .map(|hit| hit.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "s", "b"]
    );
    assert!((response.results.first().unwrap().relevance_score - 0.97).abs() < 1e-12);
    assert!((response.results.get(1).unwrap().relevance_score - 0.96).abs() < 1e-12);
    assert!((response.results.get(2).unwrap().relevance_score - 0.36).abs() < 1e-12);
    Ok(())
}

#[tokio::test]
async fn embedding_failure_is_visible_and_keyword_results_survive() -> TestResult {
    for mode in [SearchMode::Vector, SearchMode::Hybrid] {
        for provider in [
            None,
            Some(Arc::new(FakeProvider::http_errors(503))),
            Some(Arc::new(FakeProvider::new(vec![Script::Embed(vec![
                Embedding {
                    vector: vec![0.0, 0.0],
                },
            ])]))),
        ] {
            let (_dir, store) = seeded(provider).await?;
            let response = store
                .search_at(
                    query("alpha", mode),
                    Timestamp::now(),
                    &jiff::tz::TimeZone::UTC,
                )
                .await?;
            assert!(response.degraded.is_some());
            assert_ne!(response.results, []);
        }
    }
    Ok(())
}

#[tokio::test]
async fn corrupt_vectors_are_errors() -> TestResult {
    for bytes in [
        vec![0u8; 3],
        vec![0u8; 4],
        f32::NAN
            .to_le_bytes()
            .into_iter()
            .chain(0f32.to_le_bytes())
            .collect(),
    ] {
        let (_dir, store) = seeded(Some(provider())).await?;
        store
            .with_conn(move |conn| {
                conn.execute("UPDATE ros_messages SET embedding=?1 WHERE id='a'", [bytes])
                    .map_err(failed)?;
                Ok(())
            })
            .await?;
        assert!(matches!(
            store
                .search_at(
                    query("alpha", SearchMode::Vector),
                    Timestamp::now(),
                    &jiff::tz::TimeZone::UTC
                )
                .await,
            Err(SqliteError::StoredEmbedding)
        ));
    }
    Ok(())
}

#[tokio::test]
async fn regex_matches_tool_results_case_insensitively_and_rejects_bad_patterns() -> TestResult {
    let (_dir, store) = seeded(None).await?;
    store
        .with_conn(|conn| {
            conn.execute(
                "UPDATE ros_messages SET tool_result='Deploy SUCCESS 42' WHERE id='b'",
                [],
            )
            .map_err(failed)?;
            Ok(())
        })
        .await?;
    let result = store
        .search_at(
            query("^deploy.*42$", SearchMode::Regex),
            Timestamp::now(),
            &jiff::tz::TimeZone::UTC,
        )
        .await?;
    assert_eq!(
        result
            .results
            .iter()
            .map(|hit| hit.id.as_str())
            .collect::<Vec<_>>(),
        ["b"]
    );
    assert!(result.degraded.is_none());
    for pattern in ["[", "(?:a{1000}){1000}"] {
        assert!(matches!(
            store
                .search_at(
                    query(pattern, SearchMode::Regex),
                    Timestamp::now(),
                    &jiff::tz::TimeZone::UTC
                )
                .await,
            Err(SqliteError::Regex(_))
        ));
    }
    Ok(())
}

#[tokio::test]
async fn trigram_indexes_follow_inserts_updates_and_deletes() -> TestResult {
    let (_dir, store) = seeded(None).await?;
    store.with_conn(|conn| {conn.execute_batch("UPDATE ros_messages SET content='PostgreSQL',tool_result='quartz' WHERE id='a'; UPDATE ros_summaries SET content='PostgreSQL' WHERE id='s';").map_err(failed)?; Ok(())}).await?;
    let result = store
        .search_at(
            query("postgressql", SearchMode::Trigram),
            Timestamp::now(),
            &jiff::tz::TimeZone::UTC,
        )
        .await?;
    assert_eq!(result.results.len(), 2);
    let result = store
        .search_at(
            query("quarts", SearchMode::Trigram),
            Timestamp::now(),
            &jiff::tz::TimeZone::UTC,
        )
        .await?;
    assert_eq!(result.results.first().unwrap().id, "a");
    store
        .with_conn(|conn| {
            conn.execute_batch(
                "DELETE FROM ros_messages WHERE id='a'; DELETE FROM ros_summaries WHERE id='s';",
            )
            .map_err(failed)?;
            Ok(())
        })
        .await?;
    assert_eq!(
        store
            .search_at(
                query("postgressql", SearchMode::Trigram),
                Timestamp::now(),
                &jiff::tz::TimeZone::UTC
            )
            .await?
            .results,
        []
    );
    store.with_conn(|conn| {let count:u32=conn.query_row("SELECT count(*) FROM rh_messages_trigram WHERE rh_messages_trigram MATCH 'quartz'",[],|row|row.get(0)).map_err(failed)?; assert_eq!(count,0); Ok(())}).await?;
    Ok(())
}

#[tokio::test]
async fn search_uses_the_supplied_calendar_zone_and_filters() -> TestResult {
    let (_dir, store) = seeded(None).await?;
    let now = wire::time::timestamp_at_utc(2026, 10, 10, 12, 0, 0).unwrap();
    let mut request = query("alpha", SearchMode::Regex);
    request.scope = MemoryScope::Messages;
    request.window = Some(BrowseWindow::Today);
    assert_eq!(
        store
            .search_at(request.clone(), now, &jiff::tz::TimeZone::UTC)
            .await?
            .results
            .len(),
        1
    );
    let zone = jiff::tz::Offset::from_seconds(-4 * 3600)?.to_time_zone();
    assert_eq!(
        store.search_at(request.clone(), now, &zone).await?.results,
        []
    );
    request.window = None;
    request.agent = Some("another".to_owned());
    assert_eq!(
        store
            .search_at(request, now, &jiff::tz::TimeZone::UTC)
            .await?
            .results,
        []
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn query_embedding_timeout_reports_degradation() -> TestResult {
    let (_dir, store) = seeded(Some(Arc::new(FakeProvider::new(vec![Script::EmbedHang])))).await?;
    let result = store
        .search_at(
            query("alpha", SearchMode::Hybrid),
            Timestamp::now(),
            &jiff::tz::TimeZone::UTC,
        )
        .await?;
    assert!(
        result
            .degraded
            .ok_or("missing degradation")?
            .reason
            .contains("timed out")
    );
    Ok(())
}

#[tokio::test]
async fn cancelled_embedding_stops_search_and_does_not_reinforce_hits() -> TestResult {
    let (_dir, store) = seeded(None).await?;
    let cancellation = tokio_util::sync::CancellationToken::new();
    cancellation.cancel();
    let store = store.with_query_embed(memory_sqlite::QueryEmbed::new(
        Arc::new(FakeProvider::new(vec![Script::EmbedHang])),
        String::new(),
        std::time::Duration::from_secs(1),
        cancellation,
    ));
    let result = store
        .search_at(
            query("alpha", SearchMode::Hybrid),
            Timestamp::now(),
            &jiff::tz::TimeZone::UTC,
        )
        .await;
    assert!(matches!(result, Err(SqliteError::SearchCancelled)));
    store
        .with_conn(|conn| {
            let count: u32 = conn
                .query_row("SELECT sum(access_count) FROM ros_messages", [], |row| {
                    row.get(0)
                })
                .map_err(failed)?;
            assert_eq!(count, 0);
            Ok(())
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn vector_filters_scope_agent_time_and_tags() -> TestResult {
    let (_dir, store) = seeded(Some(provider())).await?;
    store.with_conn(|conn| {
        conn.execute_batch("INSERT INTO ros_tags(id,entity_type,entity_id,key,value,source,state,created_at,updated_at) VALUES ('t','summary','s','project','one','user','accepted','2026-10-10T00:00:00.000Z','2026-10-10T00:00:00.000Z')").map_err(failed)?;
        Ok(())
    }).await?;
    let mut request = query("unmatched", SearchMode::Vector);
    request.scope = MemoryScope::Summaries;
    request.agent = Some("agent".to_owned());
    request.tag = Some("project:one".parse()?);
    request.since = Some(wire::time::timestamp_at_utc(2026, 10, 10, 0, 0, 0).unwrap());
    request.before = Some(wire::time::timestamp_at_utc(2026, 10, 11, 0, 0, 0).unwrap());
    let result = store
        .search_at(request, Timestamp::now(), &jiff::tz::TimeZone::UTC)
        .await?;
    assert_eq!(
        result
            .results
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["s"]
    );
    Ok(())
}

#[tokio::test]
async fn keyword_scoring_does_not_treat_an_embedding_blob_as_query_similarity() -> TestResult {
    let (_dir, store) = seeded(None).await?;
    let now = wire::time::timestamp_at_utc(2026, 10, 10, 0, 0, 0).unwrap();
    let result = store
        .search_at(
            query("^alpha", SearchMode::Regex),
            now,
            &jiff::tz::TimeZone::UTC,
        )
        .await?;
    let first = result.results.first().ok_or("message missing")?;
    let length = u32::try_from(first.content.chars().count())?;
    let expected = kernel::memory_core::composite_score(1.0, f64::from(length) / 1000.0, 0.37);
    assert!((first.relevance_score - expected).abs() < 1e-12);
    Ok(())
}

#[tokio::test]
async fn configured_instruction_and_short_literals_reach_the_search_arms() -> TestResult {
    let (_dir, store) = seeded(None).await?;
    store.with_conn(|conn| {
        conn.execute("UPDATE ros_messages SET content='v2 search content with enough words to enter the lexical arm' WHERE id='a'",[]).map_err(failed)?;
        Ok(())
    }).await?;
    let provider = Arc::new(FakeProvider::new(vec![Script::Embed(vec![Embedding {
        vector: vec![0.0, 1.0],
    }])]));
    let store = store.with_query_embed(memory_sqlite::QueryEmbed::new(
        provider.clone(),
        "task: ".to_owned(),
        std::time::Duration::from_secs(1),
        tokio_util::sync::CancellationToken::new(),
    ));
    let now = wire::time::timestamp_at_utc(2026, 10, 10, 0, 0, 0).unwrap();
    let result = store
        .search_at(
            query("v2", SearchMode::Hybrid),
            now,
            &jiff::tz::TimeZone::UTC,
        )
        .await?;
    assert_eq!(provider.embed_requests().await, [vec!["task: v2"]]);
    let hit = result
        .results
        .iter()
        .find(|row| row.id == "a")
        .ok_or("short literal missing")?;
    let expected = 1.37 * (2.0 / 21.0 + 1.0 / 23.0);
    assert!((hit.relevance_score - expected).abs() < 1e-12);
    Ok(())
}

#[tokio::test]
async fn a_question_finds_rows_that_hold_only_some_of_its_words() -> TestResult {
    let now = wire::time::timestamp_at_utc(2026, 10, 10, 0, 0, 0).unwrap();
    for (provider, mode) in [
        (None, SearchMode::Hybrid),
        (None, SearchMode::Fts),
        (Some(provider()), SearchMode::Fts),
    ] {
        let (_dir, store) = seeded(provider).await?;
        let response = store
            .search_at(
                query("where did the alpha summary go", mode),
                now,
                &jiff::tz::TimeZone::UTC,
            )
            .await?;
        let found: Vec<&str> = response.results.iter().map(|hit| hit.id.as_str()).collect();
        assert_eq!(found, ["s", "a", "b"], "{mode}");
    }
    Ok(())
}
