use super::{
    BTreeSet, KEY, Limit, MemoryStore, PreparedSummaryRecord, PreparedSummaryTree, SECOND, SESSION,
    SearchMode, SummaryIndex, SummaryKind, SummaryPassage, SummaryStore, TestResult, Uuid, at,
    index, query, tree, vector, width,
};

pub async fn part_limit(store: &(impl MemoryStore + SummaryStore)) -> TestResult {
    let piece = "Clementine stash for the fall harvest at the community orchard. ";
    let part = Uuid::from_u128(0x123);
    let mut tree = tree()?;
    let mut indexes = std::collections::BTreeMap::from([(SESSION, index(SECOND)?)]);
    add_part(&mut tree, &mut indexes, part, piece)?;
    assert!(store.publish_live(tree, indexes).await?);
    for (text, mode) in [
        ("clementine", SearchMode::Fts),
        ("orchard meaning", SearchMode::Vector),
    ] {
        let mut q = query(text, mode);
        q.limit = Limit::clamped(Some(2), 20, 1..=50);
        q.since = Some(at(2026)?);
        q.before = Some(at(2027)?);
        q.expand = Some(false);
        let found = store.search(q).await?;
        assert_eq!(found.results.len(), 2);
        assert!(
            found
                .results
                .iter()
                .all(|hit| hit.id == part.to_string() && hit.content == piece)
        );
        assert!(
            found
                .summaries
                .values()
                .all(|detail| detail.kind == SummaryKind::Part)
        );
        assert_eq!(
            found
                .results
                .iter()
                .map(kernel::memory::MemorySearchResult::detail_id)
                .collect::<BTreeSet<_>>()
                .len(),
            2
        );
    }
    let mut q = query("clementine", SearchMode::Fts);
    q.expand = Some(false);
    let found = store.search(q.clone()).await?;
    assert_part_hits(&found, part, piece, 40);
    q.limit = Limit::clamped(Some(45), 20, 1..=50);
    let found = store.search(q).await?;
    assert_part_hits(&found, part, piece, 45);
    Ok(())
}

fn assert_part_hits(found: &kernel::memory::SearchOutcome, part: Uuid, piece: &str, limit: usize) {
    assert_eq!(found.results.len(), limit);
    assert!(
        found
            .results
            .iter()
            .all(|hit| hit.id == part.to_string() && hit.content == piece)
    );
    assert!(
        found
            .summaries
            .values()
            .all(|detail| detail.kind == SummaryKind::Part)
    );
    assert_eq!(
        found
            .results
            .iter()
            .filter_map(|hit| hit.passage.as_ref())
            .map(|passage| passage.ordinal)
            .collect::<BTreeSet<_>>()
            .len(),
        limit
    );
}

fn add_part(
    tree: &mut PreparedSummaryTree,
    indexes: &mut kernel::memory::summary_backend::SummaryIndexes,
    part: Uuid,
    piece: &str,
) -> TestResult {
    let sources = tree
        .records
        .iter()
        .find(|row| row.id == SESSION)
        .ok_or("missing root")?
        .sources
        .clone();
    let width = width(piece)?;
    let mut pieces = Vec::new();
    let vectors = [vector(1.0, 0.0), vector(0.8, 0.6)]
        .into_iter()
        .chain(std::iter::repeat_with(|| vector(0.0, 1.0)));
    for (ordinal, vector) in (0..45).zip(vectors) {
        pieces.push(SummaryPassage {
            content: piece.to_owned(),
            chars: ordinal * width..(ordinal + 1) * width,
            earliest_at: at(2024)?,
            latest_at: at(2026)?,
            vector,
        });
    }
    tree.records.push(PreparedSummaryRecord {
        id: part,
        parent: Some(SESSION),
        ordinal: 1,
        kind: SummaryKind::Part,
        content: piece.repeat(pieces.len()),
        input_key: [2; 32],
        index_key: Some(KEY),
        model: None,
        issued_part_ordinal: Some(1),
        input_tokens: Some(100),
        quality_flags: 0,
        section: None,
        sources,
    });
    tree.passage_windows.insert(
        part,
        pieces.iter().map(|piece| piece.chars.clone()).collect(),
    );
    indexes.insert(
        part,
        SummaryIndex {
            passages: pieces,
            pooled: vector(0.0, 1.0),
        },
    );
    Ok(())
}
