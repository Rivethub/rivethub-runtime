use std::sync::Arc;

use crate::Gateway;

impl Gateway {
    #[must_use]
    pub fn with_plugins(mut self, host: Arc<crate::plugins::PluginHost>) -> Self {
        self.plugins = Some(host);
        self
    }

    #[must_use]
    pub fn plugins(&self) -> Option<&Arc<crate::plugins::PluginHost>> {
        self.plugins.as_ref()
    }

    #[must_use]
    pub fn with_topics(
        mut self,
        store: Arc<dyn kernel::topics::TopicStore>,
        filing: Arc<dyn kernel::filing::FilingStore>,
    ) -> Self {
        self.topics = Some(store);
        self.filing = Some(filing);
        self
    }

    #[must_use]
    pub fn with_tenancy_store(mut self, store: Arc<dyn kernel::tenancy::TenancyStore>) -> Self {
        self.tenancy_store = Some(store);
        self
    }

    pub(crate) fn tenancy_store(
        &self,
    ) -> Result<&Arc<dyn kernel::tenancy::TenancyStore>, crate::error::ApiError> {
        self.tenancy_store.as_ref().ok_or_else(|| {
            crate::error::ApiError::Unavailable("visibility store unavailable".into())
        })
    }

    pub(crate) fn filing(
        &self,
    ) -> Result<&Arc<dyn kernel::filing::FilingStore>, crate::error::ApiError> {
        self.filing
            .as_ref()
            .ok_or_else(|| crate::error::ApiError::Unavailable("filing store unavailable".into()))
    }

    pub(crate) fn topics(
        &self,
    ) -> Result<&Arc<dyn kernel::topics::TopicStore>, crate::error::ApiError> {
        self.topics
            .as_ref()
            .ok_or_else(|| crate::error::ApiError::Unavailable("topic store unavailable".into()))
    }

    #[must_use]
    pub fn with_tidy(
        mut self,
        tidy: Arc<dyn kernel::tidy::TidyStore>,
        upkeep: Arc<dyn kernel::tidy::UpkeepStore>,
    ) -> Self {
        self.tidy = Some(tidy);
        self.upkeep = Some(upkeep);
        self
    }

    pub(crate) fn tidy(&self) -> Result<&Arc<dyn kernel::tidy::TidyStore>, crate::error::ApiError> {
        self.tidy
            .as_ref()
            .ok_or_else(|| crate::error::ApiError::Unavailable("tidy store unavailable".into()))
    }

    pub(crate) fn upkeep(
        &self,
    ) -> Result<&Arc<dyn kernel::tidy::UpkeepStore>, crate::error::ApiError> {
        self.upkeep
            .as_ref()
            .ok_or_else(|| crate::error::ApiError::Unavailable("upkeep store unavailable".into()))
    }

    #[must_use]
    pub fn with_model_assignments(mut self, assignments: Vec<crate::ModelAssignment>) -> Self {
        self.model_assignments = assignments;
        self
    }

    pub(crate) fn model_assignments(&self) -> &[crate::ModelAssignment] {
        &self.model_assignments
    }

    #[must_use]
    pub fn with_flagged(mut self, flagged: Arc<dyn kernel::flagged::FlaggedJobs>) -> Self {
        self.flagged = Some(flagged);
        self
    }

    pub(crate) fn flagged(
        &self,
    ) -> Result<&Arc<dyn kernel::flagged::FlaggedJobs>, crate::error::ApiError> {
        self.flagged
            .as_ref()
            .ok_or_else(|| crate::error::ApiError::Unavailable("flagged jobs unavailable".into()))
    }

    #[must_use]
    pub fn with_summary_watch(mut self, watch: Arc<dyn kernel::memory::SummaryWatch>) -> Self {
        self.summaries = Some(watch);
        self
    }

    #[must_use]
    pub fn with_decisions(mut self, store: Arc<dyn kernel::topics::DecisionStore>) -> Self {
        self.decisions = Some(store);
        self
    }

    pub(crate) fn decisions(
        &self,
    ) -> Result<&Arc<dyn kernel::topics::DecisionStore>, crate::error::ApiError> {
        self.decisions
            .as_ref()
            .ok_or_else(|| crate::error::ApiError::Unavailable("decision store unavailable".into()))
    }

    #[must_use]
    pub fn with_facts(mut self, store: Arc<dyn kernel::facts::FactStore>) -> Self {
        self.facts = Some(store);
        self
    }

    pub(crate) fn facts(
        &self,
    ) -> Result<&Arc<dyn kernel::facts::FactStore>, crate::error::ApiError> {
        self.facts
            .as_ref()
            .ok_or_else(|| crate::error::ApiError::Unavailable("fact store unavailable".into()))
    }

    #[must_use]
    pub fn with_review(mut self, store: Arc<dyn kernel::review::ReviewStore>) -> Self {
        self.review = Some(store);
        self
    }

    pub(crate) fn review(
        &self,
    ) -> Result<&Arc<dyn kernel::review::ReviewStore>, crate::error::ApiError> {
        self.review
            .as_ref()
            .ok_or_else(|| crate::error::ApiError::Unavailable("review store unavailable".into()))
    }

    #[must_use]
    pub fn with_topic_pages(mut self, pages: Arc<dyn kernel::topics::TopicPageStore>) -> Self {
        self.pages = Some(pages);
        self
    }

    pub(crate) fn topic_pages(&self) -> Option<&dyn kernel::topics::TopicPageStore> {
        self.pages.as_deref()
    }

    pub(crate) fn pages(
        &self,
    ) -> Result<&dyn kernel::topics::TopicPageStore, crate::error::ApiError> {
        self.pages
            .as_deref()
            .ok_or_else(|| crate::error::ApiError::Unavailable("wiki pages unavailable".into()))
    }

    #[must_use]
    pub fn with_session_deletion(
        mut self,
        deletions: Arc<dyn kernel::memory::SessionDeletion>,
    ) -> Self {
        self.deletions = Some(deletions);
        self
    }

    #[must_use]
    pub fn with_session_catalog(
        mut self,
        catalog: Arc<dyn kernel::memory::SessionCatalog>,
    ) -> Self {
        self.session_catalog = Some(catalog);
        self
    }

    #[must_use]
    pub fn with_changes(
        mut self,
        changes: Arc<dyn kernel::changes::ChangeFeed>,
        digests: Arc<dyn kernel::changes::DigestStore>,
    ) -> Self {
        self.changes = Some(changes);
        self.digests = Some(digests);
        self
    }

    pub(crate) fn changes(
        &self,
    ) -> Result<&Arc<dyn kernel::changes::ChangeFeed>, crate::error::ApiError> {
        self.changes
            .as_ref()
            .ok_or_else(|| crate::error::ApiError::Unavailable("change feed unavailable".into()))
    }

    pub(crate) fn digests(
        &self,
    ) -> Result<&Arc<dyn kernel::changes::DigestStore>, crate::error::ApiError> {
        self.digests
            .as_ref()
            .ok_or_else(|| crate::error::ApiError::Unavailable("daily digests unavailable".into()))
    }

    pub(crate) fn session_catalog(&self) -> Option<&dyn kernel::memory::SessionCatalog> {
        self.session_catalog.as_deref()
    }

    pub(crate) fn session_deletion(&self) -> Option<&dyn kernel::memory::SessionDeletion> {
        self.deletions.as_deref()
    }

    pub(crate) fn memory_export(&self) -> Option<&Arc<dyn kernel::memory::MemoryExport>> {
        self.memory_export.as_ref()
    }

    #[must_use]
    pub fn with_account(mut self, account: Arc<crate::account::AccountService>) -> Self {
        self.account = Some(account);
        self
    }

    pub(crate) fn account(
        &self,
    ) -> Result<&Arc<crate::account::AccountService>, crate::error::ApiError> {
        self.account
            .as_ref()
            .ok_or_else(|| crate::error::ApiError::Unavailable("account unavailable".into()))
    }

    pub(crate) fn count(&self, event: crate::account::UsageEvent) {
        if let Some(account) = &self.account {
            account.count(event);
        }
    }
}
