use std::sync::Arc;

use rusqlite::params;
use testkit::summary_passages::{
    CONVERSATION, KEY, LEGACY, LEGACY_CONVERSATION, NEW_KEY, OLD, SOURCE, TITLE, at, provider,
    search_and_repair, source_rows, vector,
};

use crate::search_support::{TestResult, failed, store};

async fn seed(store: &memory_sqlite::SqliteStore) -> TestResult {
    let first = at(2024)?.to_string();
    let last = at(2026)?.to_string();
    let written = at(2099)?.to_string();
    let sources = source_rows()
        .map(|(id, year)| Ok((id, at(year)?.to_string())))
        .collect::<Result<Vec<_>, jiff::Error>>()?;
    store.with_conn(move |conn| {
        for (id, key, title) in [(CONVERSATION, "orchard", Some(TITLE)), (LEGACY_CONVERSATION, "legacy", None)] {
            conn.execute(
                "INSERT INTO ros_conversations (id,session_key,agent,channel,created_at,updated_at,owner_user_id,title)
                 VALUES (?1,?2,'fixture','cli',?3,?4,'alice',?5)",
                params![id.to_string(), key, first, last, title],
            ).map_err(failed)?;
        }
        for (id, stamp) in sources {
            conn.execute(
                "INSERT INTO ros_messages (id,conversation_id,agent,channel,role,content,created_at,owner_user_id)
                 VALUES (?1,?2,'fixture','cli','user',?3,?4,'alice')",
                params![id.to_string(), CONVERSATION.to_string(), SOURCE.repeat(8), stamp],
            ).map_err(failed)?;
        }
        conn.execute(
            "INSERT INTO rh_summary_state (conversation_id,input_revision,last_input_at,settled_at,historical_only)
             VALUES (?1,1,?2,?2,0)",
            params![CONVERSATION.to_string(), last],
        ).map_err(failed)?;
        let embedding: Vec<_> = vector(0.8, 0.6).into_iter().flat_map(f32::to_le_bytes).collect();
        conn.execute(
            "INSERT INTO ros_summaries (id,conversation_id,depth,kind,content,message_count,earliest_at,latest_at,created_at,embedding)
             VALUES (?1,?2,0,'leaf',?3,1,?4,?4,?5,?6)",
            params![LEGACY.to_string(), LEGACY_CONVERSATION.to_string(), OLD, first, written, embedding],
        ).map_err(failed)?;
        Ok(())
    }).await?;
    Ok(())
}

#[tokio::test]
async fn summary_passage_search_and_repair_parity() -> TestResult {
    let (_dir, store) = store(Some(Arc::new(provider())))?;
    seed(&store).await?;
    search_and_repair(
        &store.clone().with_summary_index_key(KEY),
        &store.with_summary_index_key(NEW_KEY),
    )
    .await
}

#[tokio::test]
async fn summary_passage_replacement_parity() -> TestResult {
    let (_dir, store) = store(Some(Arc::new(provider())))?;
    seed(&store).await?;
    let store = store.with_summary_index_key(KEY);
    testkit::summary_passages::replacement(&store, async |tree| {
        store.replace_summary_tree(tree).await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn summary_passage_part_limit_parity() -> TestResult {
    let (_dir, store) = store(Some(Arc::new(provider())))?;
    seed(&store).await?;
    testkit::summary_passages::part_limit(&store.with_summary_index_key(KEY)).await
}

#[tokio::test]
async fn summary_passage_hollow_section_parity() -> TestResult {
    let (_dir, store) = store(Some(Arc::new(provider())))?;
    seed(&store).await?;
    let store = store.with_summary_index_key(KEY);
    testkit::summary_passages::hollow_section(
        &store,
        async || {
            store
                .with_conn(|conn| {
                    conn.execute_batch(
                "UPDATE ros_summaries SET source_first = 9, source_last = 9 WHERE kind = 'section';
                 CREATE TRIGGER test_hollow_section AFTER INSERT ON ros_summaries
                 WHEN new.kind = 'section'
                 BEGIN
                     UPDATE ros_summaries SET source_first = 9, source_last = 9 WHERE id = new.id;
                 END;",
            ).map_err(failed)?;
                    Ok(())
                })
                .await?;
            Ok(())
        },
        async |tree| {
            store.replace_summary_tree(tree).await?;
            Ok(())
        },
    )
    .await
}

#[tokio::test]
async fn summary_passage_partial_index_parity() -> TestResult {
    use testkit::summary_passages::{SESSION, partial_index};

    let (_dir, store) = store(Some(Arc::new(provider())))?;
    seed(&store).await?;
    let store = store.with_summary_index_key(KEY);
    partial_index(&store, async || {
        Ok(store.with_conn(|conn| {
            conn.execute(
                "UPDATE ros_summary_passages SET embedding = NULL WHERE record_id = ?1 AND ordinal = 1",
                [SESSION.to_string()],
            ).map_err(failed)?;
            conn.query_row(
                "SELECT index_needs_repair FROM ros_summaries WHERE id = ?1",
                [SESSION.to_string()],
                |row| row.get(0),
            ).map_err(failed)
        }).await?)
    }).await
}

#[tokio::test]
async fn summary_passage_inverted_times_parity() -> TestResult {
    use testkit::summary_passages::{SESSION, inverted_times};

    let (_dir, store) = store(None)?;
    seed(&store).await?;
    let store = store.with_summary_index_key(KEY);
    inverted_times(&store, async || {
        Ok(store.with_conn(|conn| {
            let insert = conn.execute(
                "INSERT INTO ros_summary_passages
                 (id, record_id, ordinal, char_start, char_end, content, earliest_at, latest_at, index_key)
                 SELECT 'inverted', record_id, 2, char_start, char_end, content, latest_at, earliest_at, index_key
                 FROM ros_summary_passages WHERE record_id = ?1 AND ordinal = 0",
                [SESSION.to_string()],
            );
            let update = conn.execute(
                "UPDATE ros_summary_passages SET latest_at = '2024-01-01T01:00:00+02:00'
                 WHERE record_id = ?1 AND ordinal = 0",
                [SESSION.to_string()],
            );
            Ok((is_check_violation(&insert), is_check_violation(&update)))
        }).await?)
    }).await
}

fn is_check_violation(result: &rusqlite::Result<usize>) -> bool {
    matches!(result, Err(rusqlite::Error::SqliteFailure(error, Some(message)))
        if error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER
            && message == "summary passage source time is inverted")
}

#[tokio::test]
async fn summary_passage_window_growth_parity() -> TestResult {
    use testkit::summary_passages::{PassageRow, window_growth};

    let (_dir, store) = store(None)?;
    seed(&store).await?;
    let store = store.with_summary_index_key(KEY);
    window_growth(&store, async |tree, key| {
        store.clone().with_summary_index_key(key).replace_summary_tree(tree).await?;
        Ok(())
    }, async || {
        let rows = store.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, content, embedding IS NOT NULL FROM ros_summary_passages ORDER BY ordinal",
            ).map_err(failed)?;
            stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?)))
                .map_err(failed)?.collect::<Result<Vec<_>, _>>().map_err(failed)
        }).await?;
        rows.into_iter().map(|(id, content, embedded)| Ok(PassageRow {
            id: id.parse()?, content, embedded,
        })).collect()
    }).await
}

#[tokio::test]
async fn summary_passage_tags_parity() -> TestResult {
    let (_dir, store) = store(Some(Arc::new(provider())))?;
    seed(&store).await?;
    testkit::summary_passages::passage_tags(&store.with_summary_index_key(KEY)).await
}
