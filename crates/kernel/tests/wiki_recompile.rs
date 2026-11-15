#![cfg(test)]

use chrono::{DateTime, Utc};
use kernel::wiki::page::{ArticleSection, WikiPage, WikiSlug};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn at() -> Result<DateTime<Utc>, chrono::ParseError> {
    "2026-10-30T12:00:00Z".parse()
}

fn page() -> Result<WikiPage, Box<dyn std::error::Error>> {
    Ok(WikiPage::empty(
        WikiSlug::try_from("acmekit".to_owned())?,
        "AcmeKit".to_owned(),
    ))
}

#[derive(Default)]
struct Recorder {
    inputs: parking_lot::Mutex<Vec<kernel::wiki::merge::MergeInput>>,
    stored: parking_lot::Mutex<Option<kernel::wiki::recompile::Recompiled>>,
    source: Option<kernel::wiki::recompile::RecompileSource>,
}

#[async_trait::async_trait]
impl kernel::wiki::merge::PageMerger for Recorder {
    async fn merge(
        &self,
        input: &kernel::wiki::merge::MergeInput,
    ) -> Result<kernel::wiki::merge::Merged, kernel::BoxError> {
        self.inputs.lock().push(input.clone());
        let newest = input.material.first().cloned().unwrap_or_default();
        Ok(kernel::wiki::merge::Merged {
            summary: if input.summary_only {
                "AcmeKit is a deploy tool; releases now ship daily.".to_owned()
            } else {
                String::new()
            },
            article: if input.summary_only {
                String::new()
            } else {
                newest
            },
            history: None,
        })
    }
}

#[async_trait::async_trait]
impl kernel::wiki::recompile::WikiRecompileStore for Recorder {
    async fn recompile_source(
        &self,
        _slug: &WikiSlug,
        _since: DateTime<Utc>,
    ) -> Result<
        Option<kernel::wiki::recompile::RecompileSource>,
        kernel::wiki::builder::WikiStoreError,
    > {
        Ok(self.source.clone())
    }

    async fn store_recompiled(
        &self,
        recompiled: kernel::wiki::recompile::Recompiled,
        _now: DateTime<Utc>,
    ) -> Result<String, kernel::wiki::builder::WikiStoreError> {
        *self.stored.lock() = Some(recompiled);
        Ok("0123456789".to_owned())
    }
}

fn summary(
    topic: Option<&str>,
    at: &str,
    text: &str,
) -> Result<kernel::wiki::recompile::SourceSummary, chrono::ParseError> {
    Ok(kernel::wiki::recompile::SourceSummary {
        topic: topic.map(str::to_owned),
        at: at.parse()?,
        content: text.to_owned(),
    })
}

#[tokio::test]
async fn consolidation_folds_the_newest_summaries_and_writes_a_lead() -> TestResult {
    const RELEASES: &str = "00000000-0000-4000-8000-0000000000a1";
    let mut current = page()?;
    current.meta.sections = vec![kernel::wiki::page::WikiSectionRef {
        topic: RELEASES.to_owned(),
        heading: "Releases".to_owned(),
    }];
    current.extra_sections = vec![ArticleSection {
        heading: "Releases".to_owned(),
        body: "Releases ship on Fridays (early October).".to_owned(),
    }];
    let store = Recorder {
        source: Some(kernel::wiki::recompile::RecompileSource {
            page: current,
            history: Vec::new(),
            summaries: vec![
                summary(
                    Some(RELEASES),
                    "2026-10-30T10:00:00Z",
                    "Releases now ship daily.",
                )?,
                summary(
                    Some(RELEASES),
                    "2026-10-05T10:00:00Z",
                    "Releases ship on Fridays.",
                )?,
                summary(None, "2026-10-29T10:00:00Z", "The build box moved.")?,
            ],
        }),
        ..Recorder::default()
    };
    let job = kernel::wiki::recompile::RecompileWiki {
        slug: WikiSlug::try_from("acmekit".to_owned())?,
        since: at()?,
    };
    let report = kernel::wiki::recompile::recompile(&store, &store, &job, at()?)
        .await?
        .ok_or("a report")?;
    let inputs = store.inputs.lock().clone();
    let section = inputs
        .iter()
        .find(|input| input.title == "AcmeKit › Releases")
        .ok_or("a section merge")?;
    assert!(
        section
            .material
            .first()
            .is_some_and(|text| text.starts_with("2026-10-30 10:00: Releases now ship daily."))
    );
    assert!(
        !section
            .material
            .iter()
            .any(|text| text.contains("build box"))
    );
    let lead = inputs.last().ok_or("a lead call")?;
    assert!(lead.summary_only);
    assert!(lead.article.contains("ship daily"));
    assert_eq!(report.merges, 2);
    let stored = store.stored.lock().clone().ok_or("stored")?;
    assert_eq!(
        stored.page.current_state,
        "AcmeKit is a deploy tool; releases now ship daily."
    );
    assert!(
        stored
            .page
            .extra_sections
            .first()
            .is_some_and(|section| section.body.contains("ship daily"))
    );
    Ok(())
}
