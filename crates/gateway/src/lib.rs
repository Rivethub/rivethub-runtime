pub mod account;
mod attach;
pub mod auth;
pub mod error;
pub mod extract;
mod history;
mod memory_body;
mod models;
pub mod plugins;
pub mod routes;
mod scope;
pub mod server;
pub mod sessions;
mod settings;
pub mod tenancy;
pub mod tls;
mod tls_resolve;

use std::sync::Arc;

use axum::Router;
use axum::middleware;
use axum::routing;

use kernel::memory::MemoryStore;
use tower_http::CompressionLevel;
use tower_http::compression::CompressionLayer;
use tower_http::compression::Predicate;
use tower_http::compression::predicate::DefaultPredicate;
use tower_http::compression::predicate::NotForContentType;

use crate::auth::DenialLog;

pub use crate::models::ModelAssignment;
pub use crate::settings::{REQUEST_DEADLINE, Settings, SettingsError, state_dir};
pub use crate::tls_resolve::private_ca;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const REVISION: Option<&str> = option_env!("RIVETHUB_BUILD_REVISION");

#[derive(Clone)]
pub(crate) struct GuardState {
    pub(crate) gateway: Arc<Gateway>,
    pub(crate) secure: bool,
}

#[expect(
    missing_debug_implementations,
    reason = "the store port is a trait object without Debug, so the derive does not compile and a hand-written impl trips the manualderive rule"
)]
pub struct Gateway {
    store: Arc<dyn MemoryStore>,
    settings: Settings,
    embed_provider: bool,
    policy: auth::OriginPolicy,
    denials: DenialLog,
    tenancy: tenancy::Tenancy,
    sessions: Arc<sessions::SessionRegistry>,
    transcript: Option<Arc<dyn kernel::transcript::TranscriptReader>>,
    session_lister: Option<Arc<dyn kernel::transcript::SessionLister>>,
    harness_setup: Option<Arc<dyn kernel::harness_setup::HarnessSetup>>,
    history: history::History,
    providers: Vec<kernel::model::ProviderHealth>,
    memory_export: Option<Arc<dyn kernel::memory::MemoryExport>>,
    topics: Option<Arc<dyn kernel::topics::TopicStore>>,
    tenancy_store: Option<Arc<dyn kernel::tenancy::TenancyStore>>,
    filing: Option<Arc<dyn kernel::filing::FilingStore>>,
    decisions: Option<Arc<dyn kernel::topics::DecisionStore>>,
    facts: Option<Arc<dyn kernel::facts::FactStore>>,
    review: Option<Arc<dyn kernel::review::ReviewStore>>,
    pages: Option<Arc<dyn kernel::topics::TopicPageStore>>,
    tidy: Option<Arc<dyn kernel::tidy::TidyStore>>,
    upkeep: Option<Arc<dyn kernel::tidy::UpkeepStore>>,
    flagged: Option<Arc<dyn kernel::flagged::FlaggedJobs>>,
    model_assignments: Vec<ModelAssignment>,
    plugins: Option<Arc<plugins::PluginHost>>,
    summaries: Option<Arc<dyn kernel::memory::SummaryWatch>>,
    deletions: Option<Arc<dyn kernel::memory::SessionDeletion>>,
    session_catalog: Option<Arc<dyn kernel::memory::SessionCatalog>>,
    changes: Option<Arc<dyn kernel::changes::ChangeFeed>>,
    digests: Option<Arc<dyn kernel::changes::DigestStore>>,
    account: Option<Arc<account::AccountService>>,
}

impl Gateway {
    #[must_use]
    pub fn new(store: Arc<dyn MemoryStore>, settings: Settings) -> Self {
        let policy = auth::OriginPolicy::new(
            settings.allowed_origins.clone(),
            settings.allowed_hosts.clone(),
        );
        Self {
            store,
            settings,
            embed_provider: false,
            policy,
            denials: DenialLog::default(),
            tenancy: tenancy::Tenancy::default(),
            sessions: Arc::new(sessions::SessionRegistry::default()),
            transcript: None,
            session_lister: None,
            harness_setup: None,
            history: history::History::default(),
            providers: Vec::new(),
            memory_export: None,
            topics: None,
            tenancy_store: None,
            filing: None,
            decisions: None,
            facts: None,
            review: None,
            pages: None,
            tidy: None,
            upkeep: None,
            flagged: None,
            model_assignments: Vec::new(),
            plugins: None,
            summaries: None,
            deletions: None,
            session_catalog: None,
            changes: None,
            digests: None,
            account: None,
        }
    }

    #[must_use]
    pub fn with_memory_export(mut self, export: Arc<dyn kernel::memory::MemoryExport>) -> Self {
        self.memory_export = Some(export);
        self
    }

    #[must_use]
    pub fn with_provider_health(mut self, providers: Vec<kernel::model::ProviderHealth>) -> Self {
        self.providers = providers;
        self
    }

    #[must_use]
    pub fn with_embed_provider(mut self) -> Self {
        self.embed_provider = true;
        self
    }

    #[must_use]
    pub fn with_tenancy(mut self, tenancy: tenancy::Tenancy) -> Self {
        self.tenancy = tenancy;
        self
    }

    #[must_use]
    pub fn with_sessions(mut self, sessions: Arc<sessions::SessionRegistry>) -> Self {
        self.sessions = sessions;
        self
    }

    pub fn sessions(&self) -> &Arc<sessions::SessionRegistry> {
        &self.sessions
    }

    #[must_use]
    pub fn with_transcript_reader(
        mut self,
        reader: Arc<dyn kernel::transcript::TranscriptReader>,
    ) -> Self {
        self.transcript = Some(reader);
        self
    }

    #[must_use]
    pub fn with_session_lister(
        mut self,
        lister: Arc<dyn kernel::transcript::SessionLister>,
    ) -> Self {
        self.session_lister = Some(lister);
        self
    }

    #[must_use]
    pub fn with_harness_setup(
        mut self,
        setup: Arc<dyn kernel::harness_setup::HarnessSetup>,
    ) -> Self {
        self.harness_setup = Some(setup);
        self
    }

    pub(crate) fn harness_setup(
        &self,
    ) -> Result<&Arc<dyn kernel::harness_setup::HarnessSetup>, error::ApiError> {
        self.harness_setup
            .as_ref()
            .ok_or_else(|| error::ApiError::Unavailable("harness setup unavailable".into()))
    }

    #[must_use]
    pub fn session_lister(&self) -> Option<&Arc<dyn kernel::transcript::SessionLister>> {
        self.session_lister.as_ref()
    }

    pub(crate) fn history(&self) -> &history::History {
        &self.history
    }

    pub async fn close(&self) {
        if let Some(host) = &self.plugins {
            host.close().await;
        }
        self.sessions.close().await;
    }

    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub(crate) fn provider_health(&self) -> &[kernel::model::ProviderHealth] {
        &self.providers
    }

    pub(crate) fn store(&self) -> &Arc<dyn MemoryStore> {
        &self.store
    }

    #[must_use]
    pub fn embeds_wired(&self) -> bool {
        self.embed_provider
    }

    pub(crate) fn policy(&self) -> &auth::OriginPolicy {
        &self.policy
    }

    pub(crate) fn denials(&self) -> &DenialLog {
        &self.denials
    }

    pub fn router(self: &Arc<Self>, secure: bool) -> Router {
        let guard_state = GuardState {
            gateway: Arc::clone(self),
            secure,
        };
        let routed = Router::new()
            .route("/healthz", routing::any(routes::healthz))
            .route(
                "/api/capture",
                routing::post(routes::capture::ingest)
                    .fallback(routes::method_not_allowed)
                    .layer(axum::extract::DefaultBodyLimit::max(
                        routes::capture_schema::body_limit(),
                    )),
            )
            .route(
                "/api/capture/status",
                routing::get(routes::capture_status::status).fallback(routes::method_not_allowed),
            )
            .route(
                "/api/harnesses/{id}/connect",
                routing::post(routes::capture_status::connect).fallback(routes::method_not_allowed),
            )
            .nest("/api/memory", routes::memory::router());
        routes::mount(routed.merge(routes::router()))
            .layer(middleware::from_fn(routes::contract_header))
            .with_state(Arc::clone(self))
            .layer(middleware::from_fn_with_state(
                Arc::clone(self),
                scope::scope_request,
            ))
            .layer(middleware::from_fn_with_state(guard_state, auth::guard))
            .layer(
                CompressionLayer::new()
                    .gzip(true)
                    .br(true)
                    .quality(CompressionLevel::Fastest)
                    .compress_when(compressible()),
            )
    }
}

fn compressible() -> impl Predicate {
    DefaultPredicate::new()
        .and(NotForContentType::const_new("audio/"))
        .and(NotForContentType::const_new("video/"))
        .and(NotForContentType::const_new("application/gzip"))
        .and(NotForContentType::const_new("application/zip"))
        .and(NotForContentType::const_new("application/octet-stream"))
        .and(NotForContentType::const_new("application/x-ndjson"))
}
