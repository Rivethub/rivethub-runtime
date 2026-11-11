use std::collections::BTreeMap;

use kernel::memory::MemoryStore;
use kernel::memory::SessionCatalog;
use kernel::memory::SessionQuery;
use kernel::memory::SessionRecord;
use kernel::memory::SessionView;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

use crate::session_catalog::BARE;
use crate::session_catalog::capture_stored;
use crate::session_catalog::seed;
use crate::session_imports::record_of;
use crate::topics::Outcome;
use crate::topics::same;

const UNKNOWN: &str = "00000000-0000-4000-8000-0000000000c7";

fn by_id(records: Vec<SessionRecord>) -> BTreeMap<String, SessionRecord> {
    records
        .into_iter()
        .map(|record| (record.id.to_string(), record))
        .collect()
}

pub async fn records_answer_bare_and_keyed_sessions_in_one_lookup(
    memory: &dyn MemoryStore,
    catalog: &dyn SessionCatalog,
) -> Outcome {
    let seeded = seed(memory).await?;
    let bare = SessionId::new(HarnessId::ClaudeCode, BARE);
    capture_stored(memory, BARE, "claude-code", "Bare start", 60).await?;
    capture_stored(memory, bare.as_str(), "claude-code", "Bare follow-up", 70).await?;
    let unknown = SessionId::new(HarnessId::Codex, UNKNOWN);
    let listed = by_id(
        catalog
            .list(&SessionQuery::new(SessionView::All))
            .await?
            .items
            .into_iter()
            .filter_map(SessionRecord::narrow)
            .collect(),
    );
    let asked = [
        bare.clone(),
        seeded.alpha.clone(),
        unknown,
        seeded.gamma.clone(),
        bare.clone(),
    ];
    let found = catalog.records(&asked).await?;
    same("each known session comes back once", found.len(), 3)?;
    let found = by_id(found);
    for session in [&bare, &seeded.alpha, &seeded.gamma] {
        let key = session.to_string();
        same(
            "a looked-up session reads as it lists",
            found.get(&key),
            listed.get(&key),
        )?;
    }
    same(
        "the bare session counts both stored keys",
        found
            .get(&bare.to_string())
            .map(|record| record.message_count),
        Some(4),
    )?;
    same(
        "get answers like a one-session lookup",
        record_of(catalog, &seeded.gamma).await?.as_ref(),
        found.get(&seeded.gamma.to_string()),
    )?;
    same(
        "an empty lookup finds nothing",
        catalog.records(&[]).await?.len(),
        0,
    )?;
    Ok(())
}
