use std::sync::Arc;

use chrono::{DateTime, Utc};
use kernel::memory::{MemoryScope, SearchMode};
use kernel::model::Embedding;
use testkit::{FakeProvider, Script};

use crate::search_support::{TestResult, failed, query, store};

const SESSION: &str = "The clementine session record contains all of the orchard planning text.";
const PART: &str = "The clementine part record contains the whole description of the work.";
const LEGACY: &str = "The clementine leaf was written recently about an older orchard project.";

async fn seed(store: &memory_sqlite::SqliteStore) -> TestResult {
    store.with_conn(|conn| {
        conn.execute_batch(
            "INSERT INTO ros_conversations (id,session_key,agent,channel,created_at,updated_at) VALUES
             ('c','orchard','fixture','cli','2024-01-01T00:00:00.000Z','2024-01-01T00:00:00.000Z');
             INSERT INTO ros_summaries (id,conversation_id,parent_id,content,kind,depth,created_at,earliest_at,latest_at,
                     pipeline_version,summary_ordinal,source_first,source_last,embedding) VALUES
             ('session','c',NULL,'The clementine session record contains all of the orchard planning text.',
               'session',2,'2026-01-01T00:00:00.000Z','2024-01-01T00:00:00.000Z','2024-01-01T00:00:00.000Z',7,0,1,1,X'0000803F00000000'),
             ('part','c','session','The clementine part record contains the whole description of the work.',
               'part',1,'2026-01-01T00:00:00.000Z','2024-02-01T00:00:00.000Z','2024-02-01T00:00:00.000Z',7,1,1,1,X'0000803F00000000'),
             ('section','c','session','clementine sectionduplicate with a vector that would otherwise match',
               'section',0,'2026-01-01T00:00:00.000Z','2024-01-01T00:00:00.000Z','2024-01-01T00:00:00.000Z',7,1,1,1,X'0000803F00000000'),
             ('legacy','c',NULL,'The clementine leaf was written recently about an older orchard project.',
               'leaf',0,'2026-01-01T00:00:00.000Z','2020-01-01T00:00:00.000Z','2020-01-01T00:00:00.000Z',6,NULL,NULL,NULL,X'0000803F00000000');",
        )
        .map_err(failed)?;
        Ok(())
    }).await?;
    Ok(())
}

fn at(text: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
    text.parse()
}

#[tokio::test]
async fn top_down_search_returns_whole_records_and_filters_by_source_time() -> TestResult {
    let (_dir, store) = store(None)?;
    seed(&store).await?;
    let now = at("2026-02-01T00:00:00Z")?;
    for mode in [SearchMode::Fts, SearchMode::Regex] {
        let mut q = query("clementine", mode);
        q.scope = MemoryScope::Summaries;
        q.before = Some(at("2025-01-01T00:00:00Z")?);
        let hits = store.search_at(q.clone(), now, &Utc).await?.results;
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|hit| hit.passage.is_none()));
        let session_at = at("2024-01-01T00:00:00Z")?;
        let part_at = at("2024-02-01T00:00:00Z")?;
        assert!(hits.iter().any(|hit| hit.id == "session"
            && hit.content == SESSION
            && hit.created_at == session_at));
        assert!(
            hits.iter()
                .any(|hit| hit.id == "part" && hit.content == PART && hit.created_at == part_at)
        );
        q.since = q.before.take();
        let hits = store.search_at(q, now, &Utc).await?.results;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits.first().expect("legacy").content, LEGACY);
    }
    assert_eq!(
        store
            .search_at(query("sectionduplicate", SearchMode::Fts), now, &Utc)
            .await?
            .results,
        []
    );
    Ok(())
}

#[tokio::test]
async fn top_down_search_scores_whole_pooled_vectors_using_source_recency() -> TestResult {
    let provider = Arc::new(FakeProvider::new(vec![Script::Embed(vec![Embedding {
        vector: vec![1.0, 0.0],
    }])]));
    let (_dir, store) = store(Some(provider))?;
    seed(&store).await?;
    let now = at("2026-01-01T00:00:00Z")?;
    let hits = store
        .search_at(query("unmatched", SearchMode::Vector), now, &Utc)
        .await?
        .results;
    assert_eq!(
        hits.iter().map(|hit| hit.id.as_str()).collect::<Vec<_>>(),
        ["legacy", "part", "session"]
    );
    let [legacy, part, session] = hits.as_slice() else {
        return Err("expected three hits".into());
    };
    assert_eq!(part.content, PART);
    assert_eq!(session.content, SESSION);
    assert!(session.relevance_score < part.relevance_score);
    assert!(part.relevance_score < legacy.relevance_score);
    Ok(())
}
