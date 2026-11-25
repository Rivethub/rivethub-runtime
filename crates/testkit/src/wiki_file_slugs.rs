use std::path::Path;

use kernel::topics::DecisionStore;
use kernel::topics::SweepOwners;
use kernel::topics::SyncTopicPage;
use kernel::topics::Topic;
use kernel::topics::sync_topic_page;
use kernel::wiki::WikiLookup;
use kernel::wiki::page::WikiSlug;
use kernel::wiki::page::normalize_slug;

use super::PageFileStore;
use super::at;
use crate::topics::Outcome;
use crate::topics::new_topic;

const SWEEP_TIME: &str = "2026-10-14T13:00:00Z";
const PROSE: &str = "The first long topic keeps this summary.";
const HAND_WRITTEN: &str = "Notes written straight into the wiki.";

pub async fn a_numbered_long_slug_keeps_the_page_it_shortens_to(
    store: &(impl PageFileStore + DecisionStore),
    wiki: &Path,
) -> Outcome {
    super::prepare_wiki(wiki)?;
    let stem = "a".repeat(80);
    let first = swept(store, new_topic(&stem, &[], None)?).await?;
    let slug = store
        .page_slug(first.id)
        .await?
        .ok_or("the first topic has no page")?;
    assert_eq!(slug.as_ref(), stem);
    let mut page = store
        .topic_page(&slug)
        .await?
        .ok_or("the first page is missing")?;
    PROSE.clone_into(&mut page.summary);
    store
        .save_topic_page(first.id, &page, at(SWEEP_TIME)?)
        .await?;

    let second = swept(store, new_topic(&format!("{stem} b"), &[], None)?).await?;

    let own = store
        .page_slug(second.id)
        .await?
        .ok_or("the second topic has no page")?;
    assert_eq!(store.page_slug(first.id).await?, Some(slug.clone()));
    let WikiLookup::Page(kept) = store.wiki_page(&slug).await? else {
        return Err("the first page row is gone".into());
    };
    assert_eq!(kept.current_state, PROSE, "the sweep blanked the first row");
    let file = std::fs::read_to_string(wiki.join("topics").join(format!("{stem}.md")))?;
    assert!(
        file.contains(PROSE),
        "the sweep blanked the first file: {file}"
    );
    assert_ne!(own, slug, "the second topic took the first page");
    assert_eq!(
        normalize_slug(own.as_ref()),
        own.as_ref(),
        "the numbered slug does not survive normalization"
    );
    let file = std::fs::read_to_string(wiki.join("topics").join(format!("{}.md", own.as_ref())))?;
    assert!(
        !file.contains(PROSE),
        "the second page shares the first file"
    );
    Ok(())
}

pub async fn a_sweep_skips_a_page_file_without_a_row(
    store: &(impl PageFileStore + DecisionStore),
    wiki: &Path,
) -> Outcome {
    super::prepare_wiki(wiki)?;
    let file = wiki.join("topics").join("orphan.md");
    std::fs::create_dir_all(wiki.join("topics"))?;
    std::fs::write(
        &file,
        format!("---\ntitle: Orphan\nslug: orphan\n---\n\n{HAND_WRITTEN}\n"),
    )?;

    let topic = swept(store, new_topic("Orphan", &[], None)?).await?;

    assert_eq!(
        store.page_slug(topic.id).await?,
        Some(WikiSlug::try_from("orphan-2".to_owned())?)
    );
    let kept = std::fs::read_to_string(&file)?;
    assert!(
        kept.contains(HAND_WRITTEN),
        "the sweep wrote over the page file: {kept}"
    );
    Ok(())
}

async fn swept(
    store: &(impl PageFileStore + DecisionStore),
    topic: Topic,
) -> Result<Topic, Box<dyn std::error::Error>> {
    let topic = store.create(topic).await?;
    let failures = sync_topic_page(
        store,
        store,
        store,
        &SyncTopicPage::topic(topic.id),
        SweepOwners::All,
        at(SWEEP_TIME)?,
    )
    .await?;
    assert!(failures.is_empty(), "the sweep failed: {failures:?}");
    Ok(topic)
}
