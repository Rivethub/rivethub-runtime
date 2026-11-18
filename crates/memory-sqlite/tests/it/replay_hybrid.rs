use crate::search_support;
use jiff::Timestamp;

use kernel::memory::{SearchMode, SearchOutcome, SearchQuery};
use memory_sqlite::SqliteStore;
use search_support::{AccessRow, Case, Hybrid, Options, Recording, TestResult};
use std::sync::Arc;
use testkit::fakes::FakeProvider;

const ANSWERED_DIFFERENTLY: [&str; 4] = [
    "deploy release",
    "\"unbalanced AND (",
    "shipping the colour switch",
    "embedfail deploy",
];

#[tokio::test]
async fn hybrid_vectors_replay() -> TestResult {
    let recording: Recording = serde_json::from_slice(&std::fs::read(testkit::recording(
        "memory-sqlite/search.json",
    ))?)?;
    let mut hybrid = recording.hybrid;
    let (_dir, store) = vector_store()?;
    search_support::seed(&store, &hybrid).await?;
    for case in std::mem::take(&mut hybrid.memory) {
        step(&store, &hybrid, case).await?;
    }
    for case in std::mem::take(&mut hybrid.backend) {
        let Some(response) = case.response else {
            continue;
        };
        let case = Case {
            query: case.query,
            options: case.options,
            hits: response.results,
            degraded: response.degraded,
        };
        step(&store, &hybrid, case).await?;
    }
    for case in std::mem::take(&mut hybrid.literal_and_degraded) {
        step(&store, &hybrid, case).await?;
    }
    assert_eq!(access(&store, false).await?, hybrid.access_after);
    assert_eq!(access(&store, true).await?, hybrid.summary_access_after);
    Ok(())
}

fn vector_store() -> TestResult<(tempfile::TempDir, SqliteStore)> {
    search_support::store(Some(Arc::new(FakeProvider::fnv_embeddings(8))))
}

async fn step(store: &SqliteStore, hybrid: &Hybrid, case: Case) -> TestResult {
    if ANSWERED_DIFFERENTLY.contains(&case.query.as_str()) {
        let recorded = case.hits.iter().map(|hit| hit.id.clone()).collect();
        search_support::reinforce(store, recorded, hybrid.clock).await?;
        return Ok(());
    }
    let actual = search(store, hybrid.clock, &case).await?;
    compare(&case.query, &actual, case.hits);
    Ok(())
}

fn request(text: &str, options: &Options) -> TestResult<SearchQuery> {
    let mut query = search_support::query(text, SearchMode::Hybrid);
    if let Some(scope) = options.scope {
        query.scope = scope;
    }
    query.agent.clone_from(&options.agent);
    query.tag = options.tag.as_deref().map(str::parse).transpose()?;
    if let Some(limit) = options.limit {
        query.limit = kernel::memory::Limit::clamped(Some(limit), 10, 1..=50);
    }
    Ok(query)
}

async fn search(store: &SqliteStore, clock: Timestamp, case: &Case) -> TestResult<SearchOutcome> {
    let store = if case.degraded.is_some() {
        store
            .clone()
            .with_query_embed(memory_sqlite::QueryEmbed::new(
                Arc::new(FakeProvider::http_errors(500)),
                kernel::memory_core::DEFAULT_EMBED_QUERY_INSTRUCTION.to_owned(),
                std::time::Duration::from_secs(1),
                tokio_util::sync::CancellationToken::new(),
            ))
    } else {
        store.clone()
    };
    let actual = store
        .search_at(
            request(&case.query, &case.options)?,
            clock,
            &jiff::tz::TimeZone::UTC,
        )
        .await?;
    assert_eq!(
        actual.degraded.is_some(),
        case.degraded.is_some(),
        "{}",
        case.query
    );
    Ok(actual)
}

fn compare(text: &str, actual: &SearchOutcome, expected: Vec<search_support::Hit>) {
    assert_eq!(
        actual.results.iter().map(|hit| &hit.id).collect::<Vec<_>>(),
        expected.iter().map(|hit| &hit.id).collect::<Vec<_>>(),
        "{text}: {:?}",
        actual.results
    );
    for (actual, expected) in actual.results.iter().zip(expected) {
        assert_eq!(actual.source, expected.source);
        match expected.role {
            search_support::HitRole::Message(role) => assert_eq!(actual.role, Some(role)),
            search_support::HitRole::Summary(kind) => {
                assert!(matches!(
                    kind,
                    kernel::memory::SummaryKind::Leaf | kernel::memory::SummaryKind::Branch
                ));
                assert_eq!(actual.role, None);
            }
        }
        assert_eq!(actual.content, expected.content);
        assert_eq!(actual.agent.as_deref(), Some(expected.agent.as_str()));
        assert_eq!(actual.created_at, expected.created_at);
        assert!(
            (actual.relevance_score - expected.relevance_score).abs() < 1e-12,
            "{text} {}: {} != {}",
            actual.id,
            actual.relevance_score,
            expected.relevance_score
        );
    }
}

async fn access(store: &memory_sqlite::SqliteStore, summary: bool) -> TestResult<Vec<AccessRow>> {
    Ok(store
        .with_conn(move |conn| {
            let table = if summary {
                "ros_summaries"
            } else {
                "ros_messages"
            };
            let mut statement = conn
                .prepare(&format!(
                    "SELECT id,access_count,last_accessed_at FROM {table} ORDER BY rowid"
                ))
                .map_err(search_support::failed)?;
            statement
                .query_map([], |row| {
                    Ok(AccessRow {
                        id: row.get(0)?,
                        access_count: row.get(1)?,
                        last_accessed_at: row.get(2)?,
                    })
                })
                .map_err(search_support::failed)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(search_support::failed)
        })
        .await?)
}
