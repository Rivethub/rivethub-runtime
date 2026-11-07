mod report;
mod scaffold;
mod tools;

use std::time::Duration;

use axum::http::{HeaderMap, HeaderName, HeaderValue};
use serde::de::DeserializeOwned;
use tokio::time::Instant;
use wire::JsonObject;
use wire::plugin::host::{PluginLogStream, PluginRecord, PluginState};
use wire::plugin::process::PluginToolResult;
use wire::plugin::view::{View, ViewRequest};
use wire::plugin::{PluginId, RouteName, ToolName};

use super::{DEFAULT_READY, PluginHost};
use tools::{InstallArgs, ScaffoldArgs, StatusArgs, TemplatesArgs, Tool};

pub(in crate::plugins) use tools::entries;

pub(super) const NAMESPACE: &str = "plugins";
const SETTLE_POLL: Duration = Duration::from_millis(100);
const SETTLE_SLACK: Duration = Duration::from_secs(2);
const VIEW_TIMEOUT: Duration = Duration::from_secs(30);
const SHOWN_ANSWER: usize = 1024;
const DEFAULT_LINES: usize = 40;
const MAX_LINES: usize = 200;
const FAILED_LINES: usize = 20;

struct PageCheck {
    title: String,
    view: RouteName,
    outcome: Result<(), String>,
}

fn decode<T: DeserializeOwned>(arguments: &JsonObject) -> Result<T, String> {
    let bytes = serde_json::to_vec(arguments).map_err(|error| error.to_string())?;
    super::parse(&bytes).map_err(|detail| format!("arguments: {detail}"))
}

fn plugin_id(raw: String) -> Result<PluginId, String> {
    PluginId::try_from(raw).map_err(|error| format!("id: {error}"))
}

impl PluginHost {
    pub(super) async fn create(&self, tool: &ToolName, arguments: &JsonObject) -> PluginToolResult {
        match self.run_creator(tool, arguments).await {
            Ok(text) => PluginToolResult {
                text,
                is_error: false,
            },
            Err(text) => PluginToolResult {
                text,
                is_error: true,
            },
        }
    }

    async fn run_creator(&self, tool: &ToolName, arguments: &JsonObject) -> Result<String, String> {
        let tool: Tool = tool
            .as_ref()
            .parse()
            .map_err(|_| format!("rh has no plug-in tool named {}", tool.as_ref()))?;
        match tool {
            Tool::Templates => {
                decode::<TemplatesArgs>(arguments)?;
                self.templates().await
            }
            Tool::Scaffold => self.scaffold(decode(arguments)?).await,
            Tool::Install => self.install_from_workshop(decode(arguments)?).await,
            Tool::Status => self.status(decode(arguments)?).await,
        }
    }

    async fn templates(&self) -> Result<String, String> {
        let templates = scaffold::templates(&self.settings.templates).await?;
        report::templates(&templates, &self.settings.templates, &self.settings.node)
            .map_err(|error| error.to_string())
    }

    async fn scaffold(&self, args: ScaffoldArgs) -> Result<String, String> {
        let id = plugin_id(args.id)?;
        let made = scaffold::write(
            &scaffold::Sources {
                templates: &self.settings.templates,
                workshop: &self.settings.workshop,
            },
            &args.template,
            &scaffold::Named {
                id: &id,
                name: args.name.trim(),
                description: args.description.trim(),
            },
        )
        .await?;
        report::scaffolded(&made, &id).map_err(|error| error.to_string())
    }

    async fn install_from_workshop(&self, args: InstallArgs) -> Result<String, String> {
        let host = self
            .me
            .upgrade()
            .ok_or_else(|| "the plug-in host is stopping".to_owned())?;
        let id = plugin_id(args.id)?;
        let folder = scaffold::folder(&self.settings.workshop, &id).await?;
        let manifest = super::registry::read_manifest(&folder)
            .await
            .map_err(|error| report::not_valid(&error))?;
        if manifest.id != id {
            return Err(format!(
                "id: the manifest names {}, but it must stay {}, its folder's name. Nothing changed.",
                manifest.id.as_ref(),
                id.as_ref()
            ));
        }
        let installed = self.peek(&id, |entry| entry.stored.path.clone());
        let (verb, done) = match installed {
            None => ("Installed", self.install(&folder).await),
            Some(path) if path == folder => ("Reloaded", host.reload(&id).await),
            Some(path) => {
                return Err(format!(
                    "{} is installed from {}, outside the plug-in workshop {}; the creator installs and reloads only plug-ins in the workshop. Nothing changed.",
                    id.as_ref(),
                    path.display(),
                    self.settings.workshop.display()
                ));
            }
        };
        let record = self
            .settle(&id, done.map_err(|error| report::not_valid(&error))?)
            .await?;
        let pages = self.check_pages(&id, &record).await;
        let lines = if matches!(record.state, PluginState::Backoff | PluginState::Invalid) {
            self.tail(&id, Some(FAILED_LINES))
                .await
                .map_err(|error| error.to_string())?
        } else {
            Vec::new()
        };
        report::installed(verb, &record, &pages, &lines).map_err(|error| error.to_string())
    }

    async fn settle(&self, id: &PluginId, record: PluginRecord) -> Result<PluginRecord, String> {
        if record.state != PluginState::Starting {
            return Ok(record);
        }
        let within = record
            .manifest
            .run
            .ready_timeout_ms
            .map_or(DEFAULT_READY, Duration::from_millis);
        let deadline = Instant::now() + within + SETTLE_SLACK;
        loop {
            tokio::time::sleep(SETTLE_POLL).await;
            let record = self.record(id).map_err(|error| error.to_string())?;
            if record.state != PluginState::Starting || Instant::now() >= deadline {
                return Ok(record);
            }
        }
    }

    async fn check_pages(&self, id: &PluginId, record: &PluginRecord) -> Vec<PageCheck> {
        let mut checks = Vec::new();
        if record.state != PluginState::Running {
            return checks;
        }
        for page in &record.manifest.contributes.pages {
            checks.push(PageCheck {
                title: page.title.clone(),
                view: page.view.clone(),
                outcome: self.check_page(id, &page.view).await,
            });
        }
        checks
    }

    async fn check_page(&self, id: &PluginId, view: &RouteName) -> Result<(), String> {
        let client = self
            .running_client(id)
            .ok_or_else(|| "it stopped before rh could ask for the view".to_owned())?;
        let url = super::plugin_url(&format!("views/{}", view.as_ref()));
        let response = client
            .post(&url)
            .headers(self.caller_headers()?)
            .timeout(VIEW_TIMEOUT)
            .json(&ViewRequest {
                params: None,
                context: wire::plugin::view::ViewContext::default(),
            })
            .send()
            .await
            .map_err(|error| format!("no answer: {error}"))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| format!("no answer: {error}"))?;
        if status != reqwest::StatusCode::OK {
            let shown = bytes
                .get(..bytes.len().min(SHOWN_ANSWER))
                .unwrap_or_default();
            let failure = format!("answered {status}: {}", String::from_utf8_lossy(shown));
            self.log(
                id,
                PluginLogStream::Host,
                format!("the {url} answer failed: {failure}"),
            );
            return Err(failure);
        }
        let checked = super::parse::<View>(&bytes).and_then(|view| super::check_view(&view));
        if let Err(detail) = &checked {
            self.log(
                id,
                PluginLogStream::Host,
                format!("refused the {url} answer: {detail}"),
            );
        }
        checked
    }

    fn caller_headers(&self) -> Result<HeaderMap, String> {
        let mut headers = HeaderMap::new();
        let owner = self.owner.get().map(|owner| owner.as_str().to_owned());
        let pairs = [
            ("x-rivethub-node", Some(self.settings.node.clone())),
            ("x-rivethub-user", owner),
            (
                "x-rivethub-request-id",
                Some(uuid::Uuid::new_v4().to_string()),
            ),
        ];
        for (name, value) in pairs {
            if let Some(value) = value {
                let value =
                    HeaderValue::from_str(&value).map_err(|error| format!("{name}: {error}"))?;
                headers.insert(HeaderName::from_static(name), value);
            }
        }
        Ok(headers)
    }

    async fn status(&self, args: StatusArgs) -> Result<String, String> {
        let Some(id) = args.id else {
            return report::listed(&self.list(), &self.settings.workshop)
                .map_err(|error| error.to_string());
        };
        let id = plugin_id(id)?;
        let record = self.record(&id).map_err(|error| error.to_string())?;
        let count = args.lines.unwrap_or(DEFAULT_LINES).clamp(1, MAX_LINES);
        let lines = self
            .tail(&id, Some(count))
            .await
            .map_err(|error| error.to_string())?;
        report::status(&record, &lines).map_err(|error| error.to_string())
    }
}
