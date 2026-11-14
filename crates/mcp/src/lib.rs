pub mod catalog;
pub mod client;
pub mod error;
pub mod memory;
mod nonblock;
pub mod plugins;
mod provenance;
mod render_memory;
mod render_search;
mod render_tags;
pub mod sessions;
pub mod tags;
mod team;
pub mod transport;
pub mod wiki;

pub use error::Error;
use kernel::memory::MemoryStore;
use kernel::plugins::PluginTools;
use kernel::tenancy::Scope;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{NotificationContext, RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler};
use serde::de::DeserializeOwned;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Server {
    instructions: Option<String>,
    memory: Arc<dyn MemoryStore>,
    tools: Arc<Vec<Tool>>,
    memory_write: bool,
    shutdown: CancellationToken,
    zone: chrono_tz::Tz,
    plugins: Option<Arc<dyn PluginTools>>,
    sessions: Option<Arc<sessions::Sessions>>,
    scope: Option<Scope>,
}

pub struct Builder {
    instructions: Option<String>,
    memory: Arc<dyn MemoryStore>,
    memory_write: bool,
    shutdown: CancellationToken,
    zone: Option<chrono_tz::Tz>,
    plugins: Option<Arc<dyn PluginTools>>,
    sessions: Option<sessions::Sessions>,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "MemoryStore and PluginTools are kernel trait objects without Debug; the formatter omits the trait objects and the tool catalog"
    )
)]
impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("memory_write", &self.memory_write)
            .field("plugins", &self.plugins.is_some())
            .field("sessions", &self.sessions.is_some())
            .finish_non_exhaustive()
    }
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "MemoryStore and PluginTools are kernel trait objects without Debug; the formatter omits the trait objects"
    )
)]
impl std::fmt::Debug for Builder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Builder")
            .field("memory_write", &self.memory_write)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, strum::EnumString)]
enum Operation {
    #[strum(serialize = "memory_search")]
    Search,
    #[strum(serialize = "memory_browse")]
    Browse,
    #[strum(serialize = "memory_team")]
    Team,
    #[strum(serialize = "memory_stats")]
    Stats,
    #[strum(serialize = "memory_get_full")]
    GetFull,
    #[strum(serialize = "memory_tags")]
    Tags,
    #[strum(serialize = "wiki_search")]
    WikiSearch,
    #[strum(serialize = "wiki_read")]
    WikiRead,
    #[strum(serialize = "session_find")]
    SessionFind,
    #[strum(serialize = "session_sanitize")]
    SessionSanitize,
    #[strum(serialize = "session_import")]
    SessionImport,
}

impl Server {
    pub fn new(
        memory: Arc<dyn MemoryStore>,
        memory_write: bool,
        shutdown: CancellationToken,
    ) -> Result<Self, Error> {
        Self::builder(memory, memory_write, shutdown).build()
    }

    pub fn builder(
        memory: Arc<dyn MemoryStore>,
        memory_write: bool,
        shutdown: CancellationToken,
    ) -> Builder {
        Builder {
            instructions: None,
            memory,
            memory_write,
            shutdown,
            zone: None,
            plugins: None,
            sessions: None,
        }
    }

    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    pub async fn call(&self, request: CallToolRequestParams) -> Result<CallToolResult, ErrorData> {
        self.dispatch(request, self.shutdown.clone()).await
    }

    async fn dispatch(
        &self,
        request: CallToolRequestParams,
        cancel: CancellationToken,
    ) -> Result<CallToolResult, ErrorData> {
        if !self.tools.iter().any(|tool| tool.name == request.name) {
            if let Some(port) = &self.plugins
                && kernel::plugins::split_tool_name(&request.name).is_ok()
            {
                let arguments = request.arguments.unwrap_or_default();
                return Ok(plugins::call(port.as_ref(), &request.name, arguments).await);
            }
            return Err(ErrorData::invalid_params(
                format!("Tool {} not found", request.name),
                None,
            ));
        }
        let operation = request.name.parse::<Operation>().map_err(|_| {
            ErrorData::invalid_params(format!("Tool {} not found", request.name), None)
        })?;
        let arguments = match request.arguments {
            Some(arguments) => arguments,
            None => rmcp::model::JsonObject::new(),
        };
        let result = match operation {
            Operation::Search
            | Operation::Browse
            | Operation::Team
            | Operation::Stats
            | Operation::GetFull
            | Operation::Tags
            | Operation::WikiSearch
            | Operation::WikiRead
            | Operation::SessionFind
            | Operation::SessionSanitize
            | Operation::SessionImport => {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => Err(Error::Cancelled),
                    result = self.execute(operation, arguments) => result,
                }
            }
        };
        match result {
            Ok(result) => Ok(result),
            Err(error) => Ok(stamp(CallToolResult::error(vec![ContentBlock::text(
                error.to_string(),
            )]))),
        }
    }

    async fn execute(
        &self,
        operation: Operation,
        arguments: rmcp::model::JsonObject,
    ) -> Result<CallToolResult, Error> {
        match operation {
            Operation::Search => {
                memory::search(self.memory.as_ref(), decode(arguments)?, self.zone).await
            }
            Operation::Browse => {
                memory::browse(self.memory.as_ref(), decode(arguments)?, self.zone).await
            }
            Operation::Team => {
                team::call(self.memory.as_ref(), decode(arguments)?, self.zone).await
            }
            Operation::Stats => memory::stats(self.memory.as_ref(), decode(arguments)?).await,
            Operation::GetFull => memory::get_full(self.memory.as_ref(), decode(arguments)?).await,
            Operation::Tags => {
                tags::call(self.memory.as_ref(), self.memory_write, decode(arguments)?).await
            }
            Operation::WikiSearch => wiki::search(self.memory.as_ref(), decode(arguments)?).await,
            Operation::WikiRead => wiki::read(self.memory.as_ref(), decode(arguments)?).await,
            Operation::SessionFind => {
                Box::pin(sessions::find(
                    self.memory.as_ref(),
                    self.sessions()?,
                    decode(arguments)?,
                    self.zone,
                ))
                .await
            }
            Operation::SessionSanitize => {
                sessions::sanitize(self.sessions()?, decode(arguments)?).await
            }
            Operation::SessionImport => {
                sessions::import(self.memory.as_ref(), self.sessions()?, decode(arguments)?).await
            }
        }
    }

    fn sessions(&self) -> Result<&sessions::Sessions, Error> {
        self.sessions.as_deref().ok_or(Error::Refused(
            "session tools are served only by rh mcp over stdio",
        ))
    }
}

impl Builder {
    pub async fn store_identity(mut self, fallback: &str) -> Result<Self, Error> {
        self.instructions = match self.memory.recall_identity().await {
            Ok(Some(identity)) => crate::team::instructions(
                identity.principal.as_deref().unwrap_or(fallback),
                &identity.people,
            ),
            Ok(None) => None,
            Err(kernel::memory::MemoryStoreError::Unavailable(error)) => {
                tracing::warn!(%error, "memory unavailable; MCP introduction omitted");
                None
            }
            Err(error) => return Err(error.into()),
        };
        Ok(self)
    }

    #[must_use]
    pub fn display_zone(mut self, zone: chrono_tz::Tz) -> Self {
        self.zone = Some(zone);
        self
    }

    #[must_use]
    pub fn plugins(mut self, port: Arc<dyn PluginTools>) -> Self {
        self.plugins = Some(port);
        self
    }

    #[must_use]
    pub fn sessions(mut self, kit: sessions::Sessions) -> Self {
        self.sessions = Some(kit);
        self
    }

    pub fn build(self) -> Result<Server, Error> {
        let mut tools = catalog::definitions()?;
        if self.sessions.is_some() {
            tools.extend(catalog::session_definitions()?);
        }
        if self.memory_write {
            let tags = tools
                .iter_mut()
                .find(|tool| tool.name == "memory_tags")
                .ok_or_else(|| Error::Invalid("memory_tags is missing from catalog".into()))?;
            tags.description = Some("Read and manage memory tags. Writes are enabled. taxonomy_upsert fully replaces aliases, parent_value, display and reason; aliases is required and omitted parent_value clears the parent.".into());
            tags.annotations = Some(rmcp::model::ToolAnnotations::from_raw(
                None,
                Some(false),
                Some(true),
                Some(false),
                Some(false),
            ));
        }
        Ok(Server {
            instructions: self.instructions,
            memory: self.memory,
            tools: Arc::new(tools),
            memory_write: self.memory_write,
            shutdown: self.shutdown,
            zone: self.zone.unwrap_or(chrono_tz::Tz::UTC),
            plugins: self.plugins,
            sessions: self.sessions.map(Arc::new),
            scope: Scope::current(),
        })
    }
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
        )
        .with_server_info(Implementation::new(
            "rivethub-mcp-server",
            env!("CARGO_PKG_VERSION"),
        ));
        info.instructions.clone_from(&self.instructions);
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let mut tools = self.tools.as_ref().clone();
        if let Some(port) = &self.plugins {
            match port.tools().await {
                Ok(list) => tools.extend(plugins::definitions(&list)),
                Err(error) => tracing::debug!(%error, "plug-in tools left out of the list"),
            }
        }
        Ok(ListToolsResult {
            tools,
            ..ListToolsResult::default()
        })
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        let Some(port) = self.plugins.clone() else {
            return;
        };
        let shutdown = self.shutdown.child_token();
        tokio::spawn(async move {
            let mut seen = match port.tools().await {
                Ok(list) => list.version,
                Err(error) => {
                    tracing::debug!(%error, "plug-in tool changes are not followed");
                    return;
                }
            };
            loop {
                let changed = tokio::select! {
                    () = shutdown.cancelled() => return,
                    changed = port.changed(seen) => changed,
                };
                match changed {
                    Ok(version) => seen = version,
                    Err(error) => {
                        tracing::debug!(%error, "plug-in tool changes stopped");
                        return;
                    }
                }
                if let Err(error) = context.peer.notify_tool_list_changed().await {
                    tracing::debug!(%error, "client left; plug-in tool changes stopped");
                    return;
                }
            }
        });
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, ErrorData> {
        let cancel = self.shutdown.child_token();
        let pending = self.dispatch(request, cancel.clone());
        let scope = self.scope.clone();
        let pending = async move {
            match scope {
                Some(scope) => scope.run(pending).await,
                None => pending.await,
            }
        };
        tokio::pin!(pending);
        tokio::select! {
            biased;
            () = context.ct.cancelled() => {}
            result = &mut pending => return result.map(Into::into),
        }
        cancel.cancel();
        match pending.await {
            Ok(result) => drop(result),
            Err(error) => drop(error),
        }
        Err(ErrorData::internal_error("request cancelled", None))
    }
}

fn decode<T: DeserializeOwned>(arguments: rmcp::model::JsonObject) -> Result<T, Error> {
    Ok(serde_json::from_value(serde_json::to_value(arguments)?)?)
}

pub(crate) fn text(value: String) -> CallToolResult {
    let mut result = CallToolResult::success(vec![ContentBlock::text(value)]);
    result.is_error = None;
    stamp(result)
}

pub(crate) fn refused(value: String) -> CallToolResult {
    stamp(CallToolResult::error(vec![ContentBlock::text(value)]))
}

fn stamp(mut result: CallToolResult) -> CallToolResult {
    let mut meta = wire::JsonObject::default();
    if meta
        .insert("contractVersion", wire::contract::MEMORY_CONTRACT)
        .is_ok()
    {
        result.meta = Some(rmcp::model::MetaObject(meta.as_map().clone()));
    }
    result
}

pub(crate) fn json<T: serde::Serialize>(value: &T) -> Result<CallToolResult, Error> {
    Ok(text(serde_json::to_string(value)?))
}
