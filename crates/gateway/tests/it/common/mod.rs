use std::net::SocketAddr;
use std::sync::Arc;

use axum::http::HeaderName;
use axum::http::HeaderValue;
use axum::http::Method;
use axum::http::Request;
use axum::http::Uri;
use gateway::Gateway;
use gateway::Settings;
use kernel::memory::BrowseFilter;
use kernel::memory::Limit;
use kernel::memory::MemoryStore;
use kernel::memory::MemoryStoreError;
use kernel::memory::SearchQuery;
use kernel::memory::TagAdd;
use kernel::memory::TagCountsQuery;
use kernel::memory::TagDecision;
use kernel::memory::TagId;
use kernel::memory::TagListQuery;
use kernel::memory::TagLookup;
use kernel::memory::TagTaxonomyDecision;
use kernel::memory::TagTaxonomyQuery;
use kernel::memory::TagTaxonomyUpsert;
use testkit::FakeMemoryStore;
use testkit::Request as Recorded;
use testkit::Response;
use tokio::sync::Semaphore;
use tokio::sync::watch;
use tower::ServiceExt;

const HOSTNAME: &str = "gold-host";
const NODE: &str = "golden-node";

pub(crate) fn settings() -> Settings {
    let scratch = std::path::PathBuf::from("/tmp/rr-gateway-unused-agents");
    Settings {
        hostname: HOSTNAME.to_string(),
        node: NODE.to_string(),
        allowed_origins: vec!["https://hub.example.test".to_string()],
        allowed_hosts: vec!["den.example.test".to_string()],
        static_dir: None,
        root_redirect: None,
        state_dir: scratch.join("state"),
        private_ca: None,
        request_deadline: gateway::REQUEST_DEADLINE,
        policy: None,
    }
}

pub(crate) fn gateway() -> (Arc<Gateway>, Arc<FakeMemoryStore>) {
    let store = Arc::new(FakeMemoryStore::default());
    let handle: Arc<dyn MemoryStore> = Arc::clone(&store) as Arc<dyn MemoryStore>;
    (Arc::new(owned(Gateway::new(handle, settings()))), store)
}

static OWNER_HOLDER: std::sync::LazyLock<Arc<gateway::local_owner::LocalOwner>> =
    std::sync::LazyLock::new(|| match gateway::local_owner::LocalOwner::unpersisted() {
        Ok(holder) => Arc::new(holder),
        Err(error) => unreachable!("random bytes for the test owner token: {error}"),
    });

pub(crate) fn local_owner() -> Arc<gateway::local_owner::LocalOwner> {
    Arc::clone(&OWNER_HOLDER)
}

pub(crate) fn owner_token() -> String {
    OWNER_HOLDER.current().expose().to_owned()
}

pub(crate) fn authorize<B>(request: &mut Request<B>) {
    let headers = request.headers_mut();
    if !headers.contains_key(axum::http::header::AUTHORIZATION)
        && let Ok(value) = HeaderValue::from_str(&format!("Bearer {}", owner_token()))
    {
        headers.insert(axum::http::header::AUTHORIZATION, value);
    }
}

pub(crate) fn owned(served: Gateway) -> Gateway {
    let served = served.with_local_owner(local_owner());
    match gateway::tenancy::UserId::new(testkit::OWNER.to_owned()) {
        Ok(owner) => served.with_tenancy(gateway::tenancy::Tenancy::local(owner)),
        Err(_) => served,
    }
}

pub(crate) fn gateway_with(settings: Settings) -> Arc<Gateway> {
    let store = Arc::new(FakeMemoryStore::default());
    let handle: Arc<dyn MemoryStore> = Arc::clone(&store) as Arc<dyn MemoryStore>;
    Arc::new(owned(Gateway::new(handle, settings)))
}

pub(crate) fn failing_gateway() -> Arc<Gateway> {
    let handle: Arc<dyn MemoryStore> = Arc::new(FailingStore);
    Arc::new(owned(Gateway::new(handle, settings())))
}

pub(crate) struct FailingStore;

fn unavailable<T>() -> Result<T, MemoryStoreError> {
    Err(MemoryStoreError::Unavailable("down".into()))
}

#[async_trait::async_trait]
impl MemoryStore for FailingStore {
    async fn close(&self) -> Result<(), MemoryStoreError> {
        Ok(())
    }

    async fn probe(&self) -> Result<(), MemoryStoreError> {
        unavailable()
    }

    fn embeddings_armed(&self) -> bool {
        false
    }

    async fn embed_model_mismatch(
        &self,
    ) -> Result<Option<kernel::memory::EmbedModelMismatch>, MemoryStoreError> {
        unavailable()
    }

    async fn wiki_topics(
        &self,
        _filter: kernel::wiki::WikiFilter,
    ) -> Result<kernel::wiki::WikiTopicList, MemoryStoreError> {
        unavailable()
    }

    async fn wiki_page(
        &self,
        _slug: &kernel::wiki::page::WikiSlug,
    ) -> Result<kernel::wiki::WikiLookup, MemoryStoreError> {
        unavailable()
    }

    async fn wiki_gaps(
        &self,
        _limit: u32,
    ) -> Result<kernel::wiki::WikiGapsResponse, MemoryStoreError> {
        unavailable()
    }

    async fn capture(
        &self,
        _batch: kernel::memory::CaptureBatchRequest,
        _owner: &kernel::owner::Owner,
        _node: Option<&str>,
    ) -> Result<kernel::memory::CaptureResult, MemoryStoreError> {
        unavailable()
    }

    async fn append(
        &self,
        _owner: &kernel::owner::Owner,
        _batch: Vec<kernel::memory::MemoryEntry>,
    ) -> Result<u64, MemoryStoreError> {
        unavailable()
    }

    async fn search(
        &self,
        _query: kernel::memory::SearchQuery,
    ) -> Result<kernel::memory::SearchOutcome, MemoryStoreError> {
        unavailable()
    }

    async fn browse(
        &self,
        _filter: kernel::memory::BrowseFilter,
    ) -> Result<Vec<kernel::memory::MemoryEntry>, MemoryStoreError> {
        unavailable()
    }

    async fn stats(&self) -> Result<kernel::memory::MemoryStatsResponse, MemoryStoreError> {
        unavailable()
    }

    async fn get_full(
        &self,
        _id: &str,
    ) -> Result<Option<kernel::memory::MemoryEntry>, MemoryStoreError> {
        unavailable()
    }

    async fn context(
        &self,
        _session: &str,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<String, MemoryStoreError> {
        unavailable()
    }

    async fn summary_dag(
        &self,
        _owner: &kernel::memory::SummaryOwner,
    ) -> Result<kernel::memory::SummaryDag, MemoryStoreError> {
        unavailable()
    }

    async fn list_tags(
        &self,
        _query: kernel::memory::TagListQuery,
    ) -> Result<Vec<kernel::memory::TagWire>, MemoryStoreError> {
        unavailable()
    }

    async fn pending_tags(
        &self,
        _limit: kernel::memory::Limit,
    ) -> Result<Vec<kernel::memory::PendingTagWire>, MemoryStoreError> {
        unavailable()
    }

    async fn tag_counts(
        &self,
        _query: kernel::memory::TagCountsQuery,
    ) -> Result<Vec<kernel::memory::TagCount>, MemoryStoreError> {
        unavailable()
    }

    async fn list_tag_taxonomy(
        &self,
        _query: kernel::memory::TagTaxonomyQuery,
    ) -> Result<Vec<kernel::memory::TagTaxonomyEntry>, MemoryStoreError> {
        unavailable()
    }

    async fn decide_tags(
        &self,
        _ids: Vec<kernel::memory::TagId>,
        _decision: kernel::memory::TagDecision,
        _decided_by: Option<String>,
    ) -> Result<u64, MemoryStoreError> {
        unavailable()
    }

    async fn decide_tag_removals(
        &self,
        _ids: Vec<kernel::memory::TagId>,
        _decision: kernel::memory::TagDecision,
        _decided_by: Option<String>,
    ) -> Result<u64, MemoryStoreError> {
        unavailable()
    }

    async fn add_tag(
        &self,
        _add: kernel::memory::TagAdd,
    ) -> Result<kernel::memory::TagWire, MemoryStoreError> {
        unavailable()
    }

    async fn lookup_tags(
        &self,
        _lookup: kernel::memory::TagLookup,
    ) -> Result<std::collections::BTreeMap<String, Vec<kernel::memory::TagWire>>, MemoryStoreError>
    {
        unavailable()
    }

    async fn wiki_imported_authors(
        &self,
        _slugs: Vec<String>,
    ) -> Result<std::collections::BTreeMap<String, Vec<kernel::memory::Provenance>>, MemoryStoreError>
    {
        unavailable()
    }

    async fn lookup_origins(
        &self,
        _session_keys: Vec<String>,
    ) -> Result<std::collections::BTreeMap<String, kernel::memory::Provenance>, MemoryStoreError>
    {
        unavailable()
    }

    async fn upsert_tag_taxonomy(
        &self,
        _upsert: kernel::memory::TagTaxonomyUpsert,
    ) -> Result<kernel::memory::TagTaxonomyEntry, MemoryStoreError> {
        unavailable()
    }

    async fn decide_tag_taxonomy(
        &self,
        _decision: kernel::memory::TagTaxonomyDecision,
    ) -> Result<u64, MemoryStoreError> {
        unavailable()
    }

    async fn merge_tag_taxonomy(
        &self,
        _merge: kernel::memory::TagTaxonomyMerge,
    ) -> Result<kernel::memory::TagTaxonomyMergeResult, MemoryStoreError> {
        unavailable()
    }

    async fn tag_job_context(
        &self,
        _summary_id: uuid::Uuid,
    ) -> Result<Option<kernel::memory::TagJobContext>, MemoryStoreError> {
        unavailable()
    }

    async fn store_tag_suggestions(
        &self,
        _write: kernel::memory::TagSuggestionWrite,
    ) -> Result<kernel::memory::TagSuggestionStored, MemoryStoreError> {
        unavailable()
    }
}

pub(crate) struct GatedStore {
    inner: FakeMemoryStore,
    pub(crate) entered: watch::Sender<u64>,
    pub(crate) release: Semaphore,
}

impl Default for GatedStore {
    fn default() -> Self {
        Self {
            inner: FakeMemoryStore::default(),
            entered: watch::channel(0).0,
            release: Semaphore::new(0),
        }
    }
}

#[async_trait::async_trait]
impl MemoryStore for GatedStore {
    async fn close(&self) -> Result<(), MemoryStoreError> {
        Ok(())
    }

    async fn probe(&self) -> Result<(), MemoryStoreError> {
        MemoryStore::probe(&self.inner).await
    }

    fn embeddings_armed(&self) -> bool {
        self.inner.embeddings_armed()
    }

    async fn embed_model_mismatch(
        &self,
    ) -> Result<Option<kernel::memory::EmbedModelMismatch>, MemoryStoreError> {
        self.inner.embed_model_mismatch().await
    }

    async fn wiki_topics(
        &self,
        filter: kernel::wiki::WikiFilter,
    ) -> Result<kernel::wiki::WikiTopicList, MemoryStoreError> {
        self.inner.wiki_topics(filter).await
    }

    async fn wiki_page(
        &self,
        slug: &kernel::wiki::page::WikiSlug,
    ) -> Result<kernel::wiki::WikiLookup, MemoryStoreError> {
        self.inner.wiki_page(slug).await
    }

    async fn wiki_gaps(
        &self,
        limit: u32,
    ) -> Result<kernel::wiki::WikiGapsResponse, MemoryStoreError> {
        self.inner.wiki_gaps(limit).await
    }

    async fn capture(
        &self,
        batch: kernel::memory::CaptureBatchRequest,
        owner: &kernel::owner::Owner,
        node: Option<&str>,
    ) -> Result<kernel::memory::CaptureResult, MemoryStoreError> {
        self.inner.capture(batch, owner, node).await
    }

    async fn append(
        &self,
        owner: &kernel::owner::Owner,
        batch: Vec<kernel::memory::MemoryEntry>,
    ) -> Result<u64, MemoryStoreError> {
        self.inner.append(owner, batch).await
    }

    async fn search(
        &self,
        query: SearchQuery,
    ) -> Result<kernel::memory::SearchOutcome, MemoryStoreError> {
        self.entered.send_modify(|count| *count += 1);
        drop(self.release.acquire().await);
        self.inner.search(query).await
    }

    async fn browse(
        &self,
        filter: BrowseFilter,
    ) -> Result<Vec<kernel::memory::MemoryEntry>, MemoryStoreError> {
        self.inner.browse(filter).await
    }

    async fn stats(&self) -> Result<kernel::memory::MemoryStatsResponse, MemoryStoreError> {
        self.inner.stats().await
    }

    async fn get_full(
        &self,
        id: &str,
    ) -> Result<Option<kernel::memory::MemoryEntry>, MemoryStoreError> {
        self.inner.get_full(id).await
    }

    async fn context(
        &self,
        session: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<String, MemoryStoreError> {
        self.inner.context(session, now).await
    }

    async fn summary_dag(
        &self,
        owner: &kernel::memory::SummaryOwner,
    ) -> Result<kernel::memory::SummaryDag, MemoryStoreError> {
        self.inner.summary_dag(owner).await
    }

    async fn list_tags(
        &self,
        query: TagListQuery,
    ) -> Result<Vec<kernel::memory::TagWire>, MemoryStoreError> {
        self.inner.list_tags(query).await
    }

    async fn pending_tags(
        &self,
        limit: Limit,
    ) -> Result<Vec<kernel::memory::PendingTagWire>, MemoryStoreError> {
        self.inner.pending_tags(limit).await
    }

    async fn tag_counts(
        &self,
        query: TagCountsQuery,
    ) -> Result<Vec<kernel::memory::TagCount>, MemoryStoreError> {
        self.inner.tag_counts(query).await
    }

    async fn list_tag_taxonomy(
        &self,
        query: TagTaxonomyQuery,
    ) -> Result<Vec<kernel::memory::TagTaxonomyEntry>, MemoryStoreError> {
        self.inner.list_tag_taxonomy(query).await
    }

    async fn decide_tags(
        &self,
        ids: Vec<TagId>,
        decision: TagDecision,
        decided_by: Option<String>,
    ) -> Result<u64, MemoryStoreError> {
        self.inner.decide_tags(ids, decision, decided_by).await
    }

    async fn decide_tag_removals(
        &self,
        ids: Vec<TagId>,
        decision: TagDecision,
        decided_by: Option<String>,
    ) -> Result<u64, MemoryStoreError> {
        self.inner
            .decide_tag_removals(ids, decision, decided_by)
            .await
    }

    async fn add_tag(&self, add: TagAdd) -> Result<kernel::memory::TagWire, MemoryStoreError> {
        self.inner.add_tag(add).await
    }

    async fn lookup_tags(
        &self,
        lookup: TagLookup,
    ) -> Result<std::collections::BTreeMap<String, Vec<kernel::memory::TagWire>>, MemoryStoreError>
    {
        self.inner.lookup_tags(lookup).await
    }

    async fn wiki_imported_authors(
        &self,
        slugs: Vec<String>,
    ) -> Result<std::collections::BTreeMap<String, Vec<kernel::memory::Provenance>>, MemoryStoreError>
    {
        self.inner.wiki_imported_authors(slugs).await
    }

    async fn lookup_origins(
        &self,
        session_keys: Vec<String>,
    ) -> Result<std::collections::BTreeMap<String, kernel::memory::Provenance>, MemoryStoreError>
    {
        self.inner.lookup_origins(session_keys).await
    }

    async fn upsert_tag_taxonomy(
        &self,
        upsert: TagTaxonomyUpsert,
    ) -> Result<kernel::memory::TagTaxonomyEntry, MemoryStoreError> {
        self.inner.upsert_tag_taxonomy(upsert).await
    }

    async fn decide_tag_taxonomy(
        &self,
        decision: TagTaxonomyDecision,
    ) -> Result<u64, MemoryStoreError> {
        self.inner.decide_tag_taxonomy(decision).await
    }

    async fn merge_tag_taxonomy(
        &self,
        merge: kernel::memory::TagTaxonomyMerge,
    ) -> Result<kernel::memory::TagTaxonomyMergeResult, MemoryStoreError> {
        self.inner.merge_tag_taxonomy(merge).await
    }

    async fn tag_job_context(
        &self,
        summary_id: uuid::Uuid,
    ) -> Result<Option<kernel::memory::TagJobContext>, MemoryStoreError> {
        self.inner.tag_job_context(summary_id).await
    }

    async fn store_tag_suggestions(
        &self,
        write: kernel::memory::TagSuggestionWrite,
    ) -> Result<kernel::memory::TagSuggestionStored, MemoryStoreError> {
        self.inner.store_tag_suggestions(write).await
    }
}

fn remote_addr(request: &Recorded) -> Result<SocketAddr, String> {
    let via = request.via.as_deref().unwrap_or_default();
    if via.contains("remote") || via.contains("127.0.0.2") {
        "127.0.0.2:41000"
            .parse()
            .map_err(|error| format!("remote test address: {error}"))
    } else {
        "127.0.0.1:41000"
            .parse()
            .map_err(|error| format!("loopback test address: {error}"))
    }
}

pub(crate) async fn answer(app: axum::Router, request: Recorded) -> Result<Response, String> {
    let addr = remote_addr(&request)?;
    let uri: Uri = request
        .target
        .parse()
        .map_err(|error| format!("recorded target {}: {error}", request.target))?;
    let method = Method::from_bytes(request.method.as_bytes())
        .map_err(|error| format!("recorded method: {error}"))?;
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in &request.headers {
        let parsed_name = HeaderName::from_bytes(name.to_ascii_lowercase().as_bytes())
            .map_err(|error| format!("header name {name}: {error}"))?;
        let parsed_value = HeaderValue::from_bytes(value.as_bytes())
            .map_err(|error| format!("header value {value}: {error}"))?;
        builder = builder.header(parsed_name, parsed_value);
    }
    let mut request = builder
        .body(axum::body::Body::from(request.body))
        .map_err(|error| format!("recorded request: {error}"))?;
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(addr));
    if gateway::auth::is_loopback_remote(addr) {
        authorize(&mut request);
    }
    let response = app
        .oneshot(request)
        .await
        .map_err(|error| format!("in-process service: {error}"))?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                String::from_utf8_lossy(value.as_bytes()).to_string(),
            )
        })
        .collect();
    let body = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .map_err(|error| format!("response body: {error}"))?
        .to_vec();
    Ok(Response {
        status,
        headers,
        body,
    })
}

const CORS_LISTED: [&str; 2] = ["https://hub.example.test", "app://bundle"];
const CORS_METHODS: &str = "GET,POST,PATCH,DELETE";
const CORS_HEADERS: &str =
    "authorization,content-type,x-rh-csrf,x-rivet-conversation,x-rivet-title";

pub(crate) fn expect_cors(case: &mut testkit::http::HttpCase) {
    let mut lines = case.request.head.lines();
    let preflight = lines
        .next()
        .is_some_and(|line| line.starts_with("OPTIONS "));
    let origin = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("origin"))
        .map(|(_, value)| value.trim().to_owned());
    case.response.headers.retain(|(name, _)| {
        let lower = name.to_ascii_lowercase();
        !lower.starts_with("access-control-") && lower != "vary"
    });
    let mut add = |name: &str, value: &str| {
        case.response
            .headers
            .push((name.to_owned(), value.to_owned()));
    };
    if let Some(origin) = origin.filter(|origin| CORS_LISTED.contains(&origin.as_str())) {
        add("Access-Control-Allow-Origin", &origin);
        add("Access-Control-Allow-Credentials", "true");
    }
    if preflight {
        add("Access-Control-Allow-Methods", CORS_METHODS);
        add("Access-Control-Allow-Headers", CORS_HEADERS);
        "HTTP/1.1 200 OK".clone_into(&mut case.response.status_line);
        case.response
            .headers
            .retain(|(name, _)| !name.eq_ignore_ascii_case("content-type"));
        case.response.body = None;
    }
}
