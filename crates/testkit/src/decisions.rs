use kernel::topics::Decision;
use kernel::topics::DecisionText;
use kernel::topics::TopicError;

use crate::topic_pages::PageStores;
use crate::topics::Outcome;
use crate::topics::at;
use crate::topics::new_topic;
use crate::topics::same;
use crate::topics::session;

fn text(words: &str) -> Result<DecisionText, TopicError> {
    DecisionText::new(words)
}

pub async fn decisions_are_the_topic_page_decisions_section(stores: PageStores<'_>) -> Outcome {
    let root = stores.create(new_topic("Mesh", &[], None)?).await?;
    let sub = stores
        .create(new_topic("Certificates", &[], Some(root.id))?)
        .await?;
    let store = stores.decisions;
    same("none yet", store.decisions(root.id).await?, Vec::new())?;
    let first = Decision::new(
        root.id,
        text("Use step-ca")?,
        "dana",
        Some(session("decide")?),
        at(1),
    );
    store.keep_decisions(std::slice::from_ref(&first)).await?;
    same("kept", store.decision(first.id).await?, Some(first.clone()))?;
    let edited = first.edit(text("Use step-ca for mesh certificates")?, at(2))?;
    let replacement = Decision::new(root.id, text("Renew every 7 days")?, "dana", None, at(3));
    let superseded = edited.supersede(Some(&replacement), at(3))?;
    store
        .keep_decisions(&[replacement.clone(), superseded.clone()])
        .await?;
    let stale = edited.edit(text("Use step-ca everywhere")?, at(4))?;
    same(
        "a stale edit leaves the superseded decision alone",
        matches!(
            store.keep_decisions(&[stale]).await,
            Err(TopicError::DecisionSuperseded { .. })
        ),
        true,
    )?;
    same(
        "listed",
        store.decisions(root.id).await?,
        vec![superseded, replacement],
    )?;
    let nested = Decision::new(sub.id, text("Keep the root offline")?, "ana", None, at(4));
    store.keep_decisions(std::slice::from_ref(&nested)).await?;
    let other = stores.create(new_topic("Fabric", &[], None)?).await?;
    let moved = Decision::new(other.id, text("One CA per mesh")?, "ana", None, at(5));
    store.keep_decisions(std::slice::from_ref(&moved)).await?;
    stores.topics.merge(other.id, root.id, at(6)).await?;
    stores.settle().await?;
    let page = stores.page(&root).await?;
    let section = page.sections.first().ok_or("the page has no sections")?;
    same(
        "decisions section",
        (
            section.topic,
            section.heading.as_str(),
            section.body.as_str(),
        ),
        (
            None,
            "Decisions",
            "- ~~Use step-ca for mesh certificates~~ superseded (dana, 2026-09-21)\n\
             - Renew every 7 days (dana, 2026-09-21)\n\
             - One CA per mesh (ana, 2026-09-21)\n\n\
             ### Certificates\n\n\
             - Keep the root offline (ana, 2026-09-21)",
        ),
    )?;
    same(
        "rendered",
        stores
            .shown("mesh")
            .await?
            .markdown
            .contains("## Decisions"),
        true,
    )?;
    Ok(())
}
