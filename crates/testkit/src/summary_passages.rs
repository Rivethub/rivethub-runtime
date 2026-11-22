mod limits;
mod mutations;

pub use limits::part_limit;
pub use mutations::{PassageRow, hollow_section, inverted_times, partial_index, window_growth};

use std::collections::BTreeSet;

use jiff::Timestamp;
use kernel::memory::summary_backend::{SummaryIndex, SummaryPassage};
use kernel::memory::summary_reference::{
    PreparedSummaryRecord, PreparedSummaryTree, SummaryManifestEntry, SummarySectionBounds,
};
use kernel::memory::{
    Limit, MemoryScope, MemoryStore, SearchMode, SearchQuery, SummaryKind, SummaryOwner,
    SummaryStore, TagAdd, TagEntity, TagEntityType, TagLiteral, VectorState,
};
use kernel::model::Embedding;
use uuid::Uuid;

use crate::{FakeProvider, Script};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

pub const KEY: [u8; 32] = [0xab; 32];
pub const NEW_KEY: [u8; 32] = [0xcd; 32];
pub const CONVERSATION: Uuid = Uuid::from_u128(0x101);
pub const LEGACY_CONVERSATION: Uuid = Uuid::from_u128(0x102);
pub const SESSION: Uuid = Uuid::from_u128(0x120);
pub const LEGACY: Uuid = Uuid::from_u128(0x140);
pub const TITLE: &str = "Orchard plumbing";
pub const FIRST: &str = "## Orchard valves\nThe café marmalade pressure valve fits the /srv/amber-cask kern-v2x9 bypass. ";
pub const SECOND: &str = "The fallback marmalade heater uses kern-v2x9 for the later outdoor work.";
pub const OLD: &str = "The previous marmalade design used a brass elbow on the side of the pipe.";
pub const SOURCE: &str =
    "Hand-seeded orchard source text with enough words to exercise passage excerpt truncation. ";

pub fn at(year: u16) -> Result<Timestamp, jiff::Error> {
    format!("{year}-01-01T00:00:00Z").parse()
}

pub fn source_rows() -> impl Iterator<Item = (Uuid, u16)> {
    [2024, 2025, 2026]
        .into_iter()
        .zip(1u128..)
        .map(|(year, id)| (Uuid::from_u128(id), year))
}

pub fn vector(first: f32, second: f32) -> Vec<f32> {
    [first, second]
        .into_iter()
        .chain(std::iter::repeat_n(0.0, 1022))
        .collect()
}

pub fn provider() -> FakeProvider {
    FakeProvider::new(
        (0..24)
            .map(|_| {
                Script::Embed(vec![Embedding {
                    vector: vector(1.0, 0.0),
                }])
            })
            .collect(),
    )
}

fn width(text: &str) -> TestResult<i32> {
    Ok(i32::try_from(text.chars().count())?)
}

fn index(second: &str) -> TestResult<SummaryIndex> {
    let split = width(FIRST)?;
    Ok(SummaryIndex {
        pooled: vector(0.0, 1.0),
        passages: vec![
            SummaryPassage {
                content: FIRST.into(),
                chars: 0..split,
                earliest_at: at(2024)?,
                latest_at: at(2025)?,
                vector: vector(1.0, 0.0),
            },
            SummaryPassage {
                content: second.into(),
                chars: split..split + width(second)?,
                earliest_at: at(2024)?,
                latest_at: at(2026)?,
                vector: vector(0.6, 0.8),
            },
        ],
    })
}

fn section(
    id: u128,
    ordinal: i32,
    heading: &str,
    first: i32,
    verified: bool,
    chars: std::ops::Range<i32>,
) -> PreparedSummaryRecord {
    PreparedSummaryRecord {
        id: Uuid::from_u128(id),
        parent: Some(SESSION),
        ordinal,
        kind: SummaryKind::Section,
        content: format!("## {heading}\nsectionunique"),
        input_key: [1; 32],
        index_key: None,
        model: None,
        issued_part_ordinal: None,
        input_tokens: None,
        quality_flags: 0,
        section: Some(SummarySectionBounds {
            first,
            last: first,
            verified,
            char_start: chars.start,
            char_end: chars.end,
        }),
        sources: Vec::new(),
    }
}

fn tree() -> TestResult<PreparedSummaryTree> {
    let content = format!("{FIRST}{SECOND}");
    let end = width(&content)?;
    Ok(PreparedSummaryTree {
        conversation: CONVERSATION,
        owner: "alice".into(),
        expected_ids: BTreeSet::new(),
        records: vec![
            PreparedSummaryRecord {
                id: SESSION,
                parent: None,
                ordinal: 0,
                kind: SummaryKind::Session,
                content,
                input_key: [1; 32],
                index_key: Some(KEY),
                model: None,
                issued_part_ordinal: None,
                input_tokens: None,
                quality_flags: 0,
                section: None,
                sources: source_rows()
                    .zip(1..)
                    .map(|((message, _), ordinal)| SummaryManifestEntry {
                        message,
                        ordinal,
                        render_key: None,
                    })
                    .collect(),
            },
            section(0x121, 1, "Orchard valves", 1, true, 0..10),
            section(0x122, 2, "Pressure setup", 2, true, 10..20),
        ],
        retained_parts: BTreeSet::new(),
        part_order: Vec::new(),
        historical: false,
        expected_revision: Some(1),
        passage_windows: [(SESSION, vec![0..width(FIRST)?, width(FIRST)?..end])].into(),
        created_at: at(2099)?,
    })
}

fn query(text: &str, mode: SearchMode) -> SearchQuery {
    SearchQuery {
        since: None,
        before: None,
        window: None,
        expand: None,
        synthesize: None,
        text: text.to_owned(),
        mode,
        scope: MemoryScope::Summaries,
        agent: None,
        role: None,
        exclude_roles: Vec::new(),
        tool_name: None,
        tag: None,
        limit: Limit::clamped(Some(40), 20, 1..=50),
    }
}

pub async fn search_and_repair(
    configured: &(impl MemoryStore + SummaryStore),
    rotated: &(impl MemoryStore + SummaryStore),
) -> TestResult {
    assert!(
        configured
            .publish_live(tree()?, [(SESSION, index(SECOND)?)].into())
            .await?
    );
    assert_state(configured, VectorState::Embedded).await?;
    assert_eq!(configured.repair_candidates(10).await?, Vec::<Uuid>::new());
    vector_order(configured).await?;
    provenance(configured).await?;
    lexical(configured).await?;
    trigram_order(configured).await?;
    repair(rotated).await?;
    Ok(())
}

async fn vector_order(store: &impl MemoryStore) -> TestResult {
    let mut q = query("orchard meaning", SearchMode::Vector);
    q.limit = Limit::clamped(Some(3), 20, 1..=50);
    let found = store.search(q).await?;
    assert_eq!(found.degraded, None);
    let [first, legacy, second] = found.results.as_slice() else {
        return Err(format!("expected three ranked hits, got {:?}", found.results).into());
    };
    assert_eq!(
        (&first.id, first.content.as_str()),
        (&SESSION.to_string(), FIRST)
    );
    assert_eq!(
        (&legacy.id, legacy.content.as_str()),
        (&LEGACY.to_string(), OLD)
    );
    assert_eq!(
        (&second.id, second.content.as_str()),
        (&SESSION.to_string(), SECOND)
    );
    assert_eq!(legacy.passage, None);
    assert_eq!(first.passage.as_ref().ok_or("first passage")?.ordinal, 0);
    assert_eq!(second.passage.as_ref().ok_or("second passage")?.ordinal, 1);
    assert_ne!(first.detail_id(), second.detail_id());
    assert_eq!(first.created_at, at(2025)?);
    assert_eq!(second.created_at, at(2026)?);
    let age = (Timestamp::now() - at(2025)?).as_secs_f64() / 86_400.0;
    let expected = kernel::memory_core::composite_score(
        1.0,
        1.0,
        kernel::memory_core::recency_boost(age, 0, 0.6),
    );
    assert!((first.relevance_score - expected).abs() < 1e-6);
    Ok(())
}

async fn provenance(store: &impl MemoryStore) -> TestResult {
    let found = store.search(query("marmalade", SearchMode::Hybrid)).await?;
    let excerpts: BTreeSet<_> = found
        .results
        .iter()
        .map(|hit| hit.content.as_str())
        .collect();
    assert_eq!(excerpts, [FIRST, SECOND, OLD].into());
    let first = found
        .results
        .iter()
        .find(|hit| hit.content == FIRST)
        .ok_or("first passage")?;
    assert_eq!(first.title.as_deref(), Some(TITLE));
    let detail = found
        .summaries
        .get(first.detail_id())
        .ok_or("first passage detail")?;
    assert_eq!(detail.kind, SummaryKind::Session);
    assert_eq!(detail.earliest_at, Some(at(2024)?));
    assert_eq!(detail.latest_at, Some(at(2025)?));
    assert_eq!(detail.children, []);
    assert_eq!(detail.sources.len(), 2);
    assert!(
        detail
            .sources
            .iter()
            .all(|source| source.content.len() == 500)
    );
    let sections = detail
        .sections
        .iter()
        .map(|section| {
            (
                section.heading.as_str(),
                section.source_first,
                section.range_verified,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sections,
        [("Orchard valves", 1, true), ("Pressure setup", 2, true)]
    );
    let second = found
        .results
        .iter()
        .find(|hit| hit.content == SECOND)
        .ok_or("second passage")?;
    let detail = found
        .summaries
        .get(second.detail_id())
        .ok_or("second passage detail")?;
    assert_eq!(detail.sections, []);
    assert_eq!(detail.sources.len(), 3);
    let mut filtered = query("orchard meaning", SearchMode::Vector);
    filtered.since = Some(at(2025)?);
    filtered.before = Some(at(2026)?);
    let found = store.search(filtered).await?;
    let [first] = found.results.as_slice() else {
        return Err("expected only older passage".into());
    };
    assert_eq!(first.content, FIRST);
    let detail = found
        .summaries
        .get(first.detail_id())
        .ok_or("filtered passage detail")?;
    assert_eq!(detail.sources.len(), 1);
    assert_eq!(
        detail.sources.first().ok_or("filtered source")?.created_at,
        Some(at(2025)?)
    );
    Ok(())
}

async fn lexical(store: &impl MemoryStore) -> TestResult {
    for (text, mode) in [
        ("/srv/amber-cask", SearchMode::Regex),
        ("/srv/amber-cask /srv/shared", SearchMode::Trigram),
        ("CAFÉ", SearchMode::Trigram),
        ("orchard valves", SearchMode::Fts),
    ] {
        let mut q = query(text, mode);
        q.expand = Some(false);
        let found = store.search(q).await?;
        let [hit] = found.results.as_slice() else {
            return Err(format!("expected one {mode} passage").into());
        };
        assert_eq!(hit.content, FIRST);
        let detail = found
            .summaries
            .get(hit.detail_id())
            .ok_or("lexical passage detail")?;
        assert_eq!(detail.sections.len(), 2);
        assert_eq!(detail.sources, []);
    }
    let found = store
        .search(query("sectionunique", SearchMode::Fts))
        .await?;
    assert_eq!(found.results, []);
    Ok(())
}

async fn trigram_order(store: &impl MemoryStore) -> TestResult {
    for (text, expected) in [
        ("(KERN-v2x9), /srv/amber-cask /srv/shared", [FIRST, SECOND]),
        ("kern-v2x9 /srv/shared", [SECOND, FIRST]),
    ] {
        let found = store.search(query(text, SearchMode::Trigram)).await?;
        assert_eq!(
            found
                .results
                .iter()
                .map(|hit| hit.content.as_str())
                .collect::<Vec<_>>(),
            expected
        );
    }
    Ok(())
}

pub async fn passage_tags(store: &(impl MemoryStore + SummaryStore)) -> TestResult {
    assert!(
        store
            .publish_live(tree()?, [(SESSION, index(SECOND)?)].into())
            .await?
    );
    let tag = TagLiteral::parse_add("project:orchard")?;
    for (entity_type, id, expected) in [
        (TagEntityType::Conversation, CONVERSATION, Vec::new()),
        (TagEntityType::Summary, SESSION, vec![FIRST]),
    ] {
        store
            .add_tag(TagAdd {
                entity_type,
                entity: TagEntity::Id(id),
                agent: None,
                tag: tag.clone(),
                display: None,
                reason: None,
                decided_by: None,
            })
            .await?;
        for (text, mode) in [
            ("orchard valves", SearchMode::Fts),
            ("orchard meaning", SearchMode::Vector),
        ] {
            let mut q = query(text, mode);
            q.tag = Some(tag.clone());
            q.limit = Limit::clamped(Some(1), 20, 1..=50);
            let found = store.search(q).await?;
            assert_eq!(
                found
                    .results
                    .iter()
                    .map(|hit| hit.content.as_str())
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }
    Ok(())
}

async fn assert_state(store: &impl MemoryStore, expected: VectorState) -> TestResult {
    let dag = store
        .summary_dag(&SummaryOwner::Conversation(CONVERSATION))
        .await?;
    let root = dag
        .summaries
        .iter()
        .find(|node| node.id == SESSION.to_string())
        .ok_or("session missing")?;
    assert_eq!(root.vector, expected);
    Ok(())
}

async fn repair(store: &(impl MemoryStore + SummaryStore)) -> TestResult {
    assert_state(store, VectorState::Pending).await?;
    assert_eq!(store.repair_candidates(10).await?, [SESSION]);
    let found = store
        .search(query("orchard meaning", SearchMode::Vector))
        .await?;
    let [legacy] = found.results.as_slice() else {
        return Err("expected legacy only with a different index key".into());
    };
    assert_eq!(legacy.id, LEGACY.to_string());

    let expected = store
        .repair_input(SESSION)
        .await?
        .ok_or("repair input missing")?;
    assert_eq!(expected.sources.len(), 3);
    assert_eq!(expected.sections.len(), 2);
    assert!(
        store
            .repair_index(expected, index("invalid text")?)
            .await
            .is_err()
    );
    assert_state(store, VectorState::Pending).await?;
    let expected = store
        .repair_input(SESSION)
        .await?
        .ok_or("repair input missing")?;
    store.repair_index(expected, index(SECOND)?).await?;
    assert_state(store, VectorState::Embedded).await?;
    assert_eq!(store.repair_candidates(10).await?, Vec::<Uuid>::new());
    let found = store
        .search(query("orchard meaning", SearchMode::Vector))
        .await?;
    assert_eq!(
        found
            .results
            .iter()
            .map(|hit| hit.content.as_str())
            .collect::<Vec<_>>(),
        [FIRST, OLD, SECOND]
    );
    Ok(())
}

pub async fn replacement(
    store: &(impl MemoryStore + SummaryStore),
    replace: impl AsyncFn(PreparedSummaryTree) -> TestResult,
) -> TestResult {
    assert!(
        store
            .publish_live(tree()?, [(SESSION, index(SECOND)?)].into())
            .await?
    );
    let old = first_passage(store).await?;
    let next = Uuid::from_u128(0x130);
    replace(replacement_tree(SESSION, next, SECOND)?).await?;
    assert_eq!(first_passage(store).await?, old);

    let revision = Uuid::from_u128(0x131);
    let changed = "The replacement marmalade heater now uses a ceramic fitting.";
    replace(replacement_tree(next, revision, changed)?).await?;
    assert_eq!(store.repair_candidates(10).await?, [revision]);
    let found = store
        .search(query("orchard meaning", SearchMode::Vector))
        .await?;
    let [legacy] = found.results.as_slice() else {
        return Err("expected legacy while new content is pending".into());
    };
    assert_eq!(legacy.id, LEGACY.to_string());
    let lexical = store
        .search(query("ceramic fitting", SearchMode::Fts))
        .await?;
    assert_eq!(lexical.results, []);
    let expected = store
        .repair_input(revision)
        .await?
        .ok_or("content repair missing")?;
    assert_eq!(expected.content, format!("{FIRST}{changed}"));
    store.repair_index(expected, index(changed)?).await?;
    let found = store
        .search(query("orchard meaning", SearchMode::Vector))
        .await?;
    assert_eq!(
        found
            .results
            .iter()
            .map(|hit| hit.content.as_str())
            .collect::<Vec<_>>(),
        [FIRST, OLD, changed]
    );
    Ok(())
}

fn replacement_tree(previous: Uuid, next: Uuid, second: &str) -> TestResult<PreparedSummaryTree> {
    let mut tree = tree()?;
    let first_section = Uuid::from_u128(0x121);
    let second_section = Uuid::from_u128(0x122);
    tree.expected_ids = [previous, first_section, second_section].into();
    let root = tree
        .records
        .iter_mut()
        .find(|row| row.id == SESSION)
        .ok_or("missing root")?;
    root.id = next;
    root.content = format!("{FIRST}{second}");
    tree.records.retain(|row| row.kind == SummaryKind::Session);
    let mut first = section(0x125, 1, "Orchard valves", 1, true, 0..10);
    first.parent = Some(next);
    let mut second_section = section(0x126, 2, "Pressure setup", 2, true, 10..20);
    second_section.parent = Some(next);
    tree.records.extend([first, second_section]);
    tree.passage_windows = [(
        next,
        vec![
            0..width(FIRST)?,
            width(FIRST)?..width(&format!("{FIRST}{second}"))?,
        ],
    )]
    .into();
    Ok(tree)
}

async fn first_passage(store: &impl MemoryStore) -> TestResult<String> {
    let found = store
        .search(query("orchard meaning", SearchMode::Vector))
        .await?;
    let first = found.results.first().ok_or("first passage missing")?;
    assert_eq!(first.content, FIRST);
    Ok(first.detail_id().to_owned())
}
