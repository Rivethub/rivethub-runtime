use std::sync::Arc;

use kernel::changes::{ChangeFeed, DigestStore};
use kernel::facts::FactStore;
use kernel::filing::FilingStore;
use kernel::flagged::FlaggedJobs;
use kernel::memory::{SessionCatalog, SessionDeletion, SummaryWatch};
use kernel::model_calls::ModelCallStore;
use kernel::review::ReviewStore;
use kernel::tenancy::TenancyStore;
use kernel::tidy::{TidyStore, UpkeepStore};
use kernel::topics::{DecisionStore, TopicPageStore, TopicStore};

#[derive(Clone)]
#[expect(
    missing_debug_implementations,
    reason = "every field is a port trait object without Debug"
)]
pub struct StorePorts {
    topics: Arc<dyn TopicStore>,
    filing: Arc<dyn FilingStore>,
    tenancy: Arc<dyn TenancyStore>,
    decisions: Arc<dyn DecisionStore>,
    facts: Arc<dyn FactStore>,
    review: Arc<dyn ReviewStore>,
    pages: Arc<dyn TopicPageStore>,
    tidy: Arc<dyn TidyStore>,
    upkeep: Arc<dyn UpkeepStore>,
    flagged: Arc<dyn FlaggedJobs>,
    summaries: Arc<dyn SummaryWatch>,
    deletions: Arc<dyn SessionDeletion>,
    catalog: Arc<dyn SessionCatalog>,
    changes: Arc<dyn ChangeFeed>,
    digests: Arc<dyn DigestStore>,
    model_calls: Arc<dyn ModelCallStore>,
}

impl StorePorts {
    #[must_use]
    pub fn of<S>(store: &Arc<S>, tenancy: Arc<dyn TenancyStore>) -> Self
    where
        S: TopicStore
            + FilingStore
            + DecisionStore
            + FactStore
            + ReviewStore
            + TopicPageStore
            + TidyStore
            + UpkeepStore
            + FlaggedJobs
            + SummaryWatch
            + SessionDeletion
            + SessionCatalog
            + ChangeFeed
            + DigestStore
            + ModelCallStore
            + 'static,
    {
        Self {
            topics: store.clone(),
            filing: store.clone(),
            tenancy,
            decisions: store.clone(),
            facts: store.clone(),
            review: store.clone(),
            pages: store.clone(),
            tidy: store.clone(),
            upkeep: store.clone(),
            flagged: store.clone(),
            summaries: store.clone(),
            deletions: store.clone(),
            catalog: store.clone(),
            changes: store.clone(),
            digests: store.clone(),
            model_calls: store.clone(),
        }
    }

    #[must_use]
    pub fn tenancy(&self) -> &Arc<dyn TenancyStore> {
        &self.tenancy
    }
}

impl crate::Gateway {
    #[must_use]
    pub fn with_store_ports(self, ports: StorePorts) -> Self {
        let StorePorts {
            topics,
            filing,
            tenancy,
            decisions,
            facts,
            review,
            pages,
            tidy,
            upkeep,
            flagged,
            summaries,
            deletions,
            catalog,
            changes,
            digests,
            model_calls,
        } = ports;
        self.with_topics(topics, filing)
            .with_tenancy_store(tenancy)
            .with_decisions(decisions)
            .with_facts(facts)
            .with_review(review)
            .with_topic_pages(pages)
            .with_tidy(tidy, upkeep)
            .with_flagged(flagged)
            .with_summary_watch(summaries)
            .with_session_deletion(deletions)
            .with_session_catalog(catalog)
            .with_changes(changes, digests)
            .with_model_calls(model_calls)
    }
}
