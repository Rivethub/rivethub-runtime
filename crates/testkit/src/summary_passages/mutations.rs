use super::{
    CONVERSATION, FIRST, KEY, Limit, MemoryStore, NEW_KEY, OLD, PreparedSummaryTree, SECOND,
    SESSION, SearchMode, SummaryKind, SummaryStore, TestResult, Uuid, VectorState, assert_state,
    at, first_passage, index, query, replacement_tree, tree, width,
};

pub async fn partial_index(
    store: &(impl MemoryStore + SummaryStore),
    drop_embedding: impl AsyncFn() -> TestResult<bool>,
) -> TestResult {
    assert!(
        store
            .publish_live(tree()?, [(SESSION, index(SECOND)?)].into())
            .await?
    );
    let needs_repair = drop_embedding().await?;
    assert_state(store, VectorState::Pending).await?;
    let found = store
        .search(query("orchard meaning", SearchMode::Vector))
        .await?;
    assert_eq!(
        found
            .results
            .iter()
            .map(|hit| hit.content.as_str())
            .collect::<Vec<_>>(),
        [FIRST, OLD]
    );
    let found = store
        .search(query("fallback heater", SearchMode::Fts))
        .await?;
    let [hit] = found.results.as_slice() else {
        return Err("expected the lexical passage with no embedding".into());
    };
    assert_eq!(hit.content, SECOND);
    assert_eq!(hit.id, SESSION.to_string());
    assert!(!needs_repair);
    assert_eq!(store.repair_candidates(10).await?, Vec::<Uuid>::new());
    assert!(store.repair_input(SESSION).await?.is_none());
    Ok(())
}

pub async fn inverted_times(
    store: &impl SummaryStore,
    rejected_writes: impl AsyncFn() -> TestResult<(bool, bool)>,
) -> TestResult {
    assert!(
        store
            .publish_live(tree()?, [(SESSION, index(SECOND)?)].into())
            .await?
    );
    assert_eq!(rejected_writes().await?, (true, true));
    assert_eq!(store.repair_candidates(10).await?, Vec::<Uuid>::new());
    Ok(())
}

pub async fn hollow_section(
    store: &(impl MemoryStore + SummaryStore),
    hollow_out: impl AsyncFn() -> TestResult,
    replace: impl AsyncFn(PreparedSummaryTree) -> TestResult,
) -> TestResult {
    assert!(
        store
            .publish_live(tree()?, [(SESSION, index(SECOND)?)].into())
            .await?
    );
    let original = first_passage(store).await?;
    hollow_out().await?;
    let next = Uuid::from_u128(0x130);
    let mut tree = replacement_tree(SESSION, next, SECOND)?;
    tree.records.retain(|row| row.kind == SummaryKind::Session);
    replace(tree).await?;
    assert_eq!(first_passage(store).await?, original);
    let with_sections = Uuid::from_u128(0x131);
    let mut restored = replacement_tree(next, with_sections, SECOND)?;
    restored.expected_ids = [next].into();
    replace(restored).await?;
    assert_eq!(first_passage(store).await?, original);

    let reordered = Uuid::from_u128(0x132);
    replace(reordered_members(with_sections, reordered)?).await?;
    assert_ne!(first_passage(store).await?, original);

    let mut query = query("orchard meaning", SearchMode::Vector);
    query.limit = Limit::clamped(Some(1), 20, 1..=50);
    let found = store.search(query).await?;
    assert_eq!(
        found.results.first().ok_or("first passage")?.created_at,
        at(2026)?
    );
    Ok(())
}

fn reordered_members(previous: Uuid, next: Uuid) -> TestResult<PreparedSummaryTree> {
    let mut tree = replacement_tree(previous, next, SECOND)?;
    tree.expected_ids = [previous, Uuid::from_u128(0x125), Uuid::from_u128(0x126)].into();
    for (record, id) in tree
        .records
        .iter_mut()
        .filter(|row| row.kind == SummaryKind::Section)
        .zip([0x127, 0x128])
    {
        record.id = Uuid::from_u128(id);
    }
    let root = tree
        .records
        .iter_mut()
        .find(|row| row.id == next)
        .ok_or("missing root")?;
    for source in &mut root.sources {
        source.ordinal = 4 - source.ordinal;
    }
    Ok(tree)
}

#[derive(Debug, PartialEq, Eq)]
pub struct PassageRow {
    pub id: Uuid,
    pub content: String,
    pub embedded: bool,
}

pub async fn window_growth(
    store: &impl SummaryStore,
    replace: impl AsyncFn(PreparedSummaryTree, [u8; 32]) -> TestResult,
    passages: impl AsyncFn() -> TestResult<Vec<PassageRow>>,
) -> TestResult {
    assert!(
        store
            .publish_live(tree()?, [(SESSION, index(SECOND)?)].into())
            .await?
    );
    let original_messages = transcript(store).await?;
    let before = passages().await?;
    assert_eq!(before.len(), 2);
    assert!(before.iter().all(|row| row.embedded));
    let next = Uuid::from_u128(0x130);
    replace(grown_tree(SESSION, next, KEY)?, KEY).await?;
    let after = passages().await?;
    let (prefix, added) = after
        .split_last()
        .map(|(last, prefix)| (prefix, last))
        .ok_or("no passages")?;
    assert_eq!(prefix, before);
    assert_eq!(added.content, ADDED);
    assert!(!added.embedded);

    let rotated = Uuid::from_u128(0x131);
    replace(grown_tree(next, rotated, NEW_KEY)?, NEW_KEY).await?;
    let rotated_rows = passages().await?;
    assert_eq!(
        rotated_rows
            .iter()
            .map(|row| row.content.as_str())
            .collect::<Vec<_>>(),
        [FIRST, SECOND, ADDED]
    );
    assert!(
        rotated_rows
            .iter()
            .all(|row| !row.embedded && after.iter().all(|old| old.id != row.id))
    );
    assert_eq!(transcript(store).await?, original_messages);
    Ok(())
}

async fn transcript(store: &impl SummaryStore) -> TestResult<Vec<kernel::memory::WindowMessage>> {
    Ok(store
        .conversation(CONVERSATION)
        .await?
        .ok_or("missing transcript")?
        .messages)
}

const ADDED: &str = " The new window covers a spare nut.";

fn grown_tree(previous: Uuid, next: Uuid, key: [u8; 32]) -> TestResult<PreparedSummaryTree> {
    let mut tree = replacement_tree(previous, next, SECOND)?;
    let root = tree
        .records
        .iter_mut()
        .find(|row| row.id == next)
        .ok_or("missing root")?;
    let start = width(&root.content)?;
    root.content.push_str(ADDED);
    root.index_key = Some(key);
    tree.passage_windows
        .get_mut(&next)
        .ok_or("missing windows")?
        .push(start..width(&root.content)?);
    Ok(tree)
}
