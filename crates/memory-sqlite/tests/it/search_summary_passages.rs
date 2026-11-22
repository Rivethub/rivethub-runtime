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
