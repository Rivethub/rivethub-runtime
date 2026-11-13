use super::CurrentText;
use super::TopicTarget;
use super::WikiRouter;
use crate::topics::TopicPageError;
use crate::wiki::builder::WikiSummary;
use crate::wiki::page::SUMMARY_LIMIT;
use crate::wiki::page::bound::ARTICLE_BUDGET;
use crate::wiki::page::bound::SECTION_BUDGET;
use crate::wiki::page::bound::fit;
use crate::wiki::page::cap_summary;

impl WikiRouter<'_> {
    pub async fn same_route(
        &self,
        lead: &TopicTarget,
        summary: &WikiSummary,
    ) -> Result<bool, TopicPageError> {
        let Some(waits) = self.topics.waits_on(summary).await? else {
            return Ok(false);
        };
        Ok(self
            .topics
            .filed_target(&waits, self.now)
            .await?
            .is_some_and(|target| {
                target.page_slug == lead.page_slug && target.section == lead.section
            }))
    }

    pub(super) async fn with_current(
        &self,
        mut target: TopicTarget,
    ) -> Result<TopicTarget, TopicPageError> {
        let Some(page) = self.topics.pages.topic_page(&target.page_slug).await? else {
            return Ok(target);
        };
        target.current = Some(match &target.section {
            None => CurrentText {
                summary: cap_summary(&page.summary, SUMMARY_LIMIT).kept,
                article: Some(fit(&page.article, ARTICLE_BUDGET).kept),
                section: None,
            },
            Some(_) => CurrentText {
                summary: String::new(),
                article: None,
                section: Some(
                    page.sections
                        .iter()
                        .find(|section| section.topic == Some(target.topic))
                        .map(|section| fit(&section.body, SECTION_BUDGET).kept)
                        .unwrap_or_default(),
                ),
            },
        });
        Ok(target)
    }
}
