use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kernel::plugins::{
    self, PluginToolCall, PluginToolError, PluginToolHint, PluginToolList, PluginToolResult,
    PluginTools,
};
use rmcp::model::{CallToolResult, ContentBlock, Tool, ToolAnnotations};
use wire::plugin::{PluginId, ToolName, host, manifest, process};

const LONG_POLL: Duration = Duration::from_secs(90);
const CALL_TIMEOUT: Duration = Duration::from_secs(130);

pub(crate) fn definitions(list: &PluginToolList) -> Vec<Tool> {
    list.tools
        .iter()
        .map(|entry| {
            let tool = &entry.tool;
            let hinted = |hint| Some(tool.hints.contains(&hint));
            let mut definition = Tool::new(
                kernel::plugins::tool_name(&entry.plugin, &tool.name),
                tool.description.clone(),
                Arc::new(tool.input_schema.as_map().clone()),
            );
            definition.title.clone_from(&tool.title);
            definition.annotations = Some(ToolAnnotations::from_raw(
                tool.title.clone(),
                hinted(PluginToolHint::ReadOnly),
                hinted(PluginToolHint::Destructive),
                hinted(PluginToolHint::Idempotent),
                hinted(PluginToolHint::OpenWorld),
            ));
            definition
        })
        .collect()
}

const TOOL_USE_META: &str = "claudecode/toolUseId";
const TOOL_USE_HEADER: &str = "x-rivethub-tool-use";

pub(crate) fn tool_use(meta: &rmcp::model::RequestMetaObject) -> Option<String> {
    meta.get(TOOL_USE_META)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

pub(crate) async fn call(
    port: &dyn PluginTools,
    name: &str,
    arguments: rmcp::model::JsonObject,
    tool_use: Option<&str>,
) -> CallToolResult {
    let answered = match kernel::plugins::split_tool_name(name) {
        Ok((plugin, tool)) => match wire::JsonObject::from_serialize(&arguments) {
            Ok(arguments) => {
                port.call(
                    &plugin,
                    &tool,
                    PluginToolCall {
                        arguments,
                        session_id: None,
                    },
                    tool_use,
                )
                .await
            }
            Err(error) => Err(PluginToolError::Failed {
                plugin: plugin.as_ref().to_owned(),
                tool: tool.as_ref().to_owned(),
                detail: error.to_string(),
            }),
        },
        Err(error) => Err(error),
    };
    match answered {
        Ok(result) => {
            let mut answer = CallToolResult::success(vec![ContentBlock::text(result.text)]);
            answer.is_error = Some(result.is_error);
            crate::stamp(answer)
        }
        Err(error) => crate::stamp(CallToolResult::error(vec![ContentBlock::text(
            error.to_string(),
        )])),
    }
}

#[derive(Debug, Clone)]
pub struct SocketTools {
    http: reqwest::Client,
    token: PathBuf,
}

impl SocketTools {
    pub fn new(socket: PathBuf, token: PathBuf) -> Result<Self, crate::Error> {
        let http = reqwest::Client::builder()
            .unix_socket(socket)
            .build()
            .map_err(|error| crate::Error::Invalid(format!("plug-in tools socket: {error}")))?;
        Ok(Self { http, token })
    }

    async fn bearer(&self) -> Result<String, PluginToolError> {
        tokio::fs::read_to_string(&self.token)
            .await
            .map(|token| token.trim().to_owned())
            .map_err(|error| {
                PluginToolError::Unreachable(format!("{}: {error}", self.token.display()))
            })
    }

    async fn list(
        &self,
        after: Option<u64>,
        wait: Duration,
    ) -> Result<PluginToolList, PluginToolError> {
        let url = match after {
            Some(seen) => format!("http://plugins/tools?after={seen}"),
            None => "http://plugins/tools".to_owned(),
        };
        let response = self
            .http
            .get(url)
            .bearer_auth(self.bearer().await?)
            .timeout(wait)
            .send()
            .await
            .map_err(|error| PluginToolError::Unreachable(error.to_string()))?;
        if !response.status().is_success() {
            return Err(PluginToolError::Unreachable(format!(
                "the tools socket answered {}",
                response.status()
            )));
        }
        response
            .json::<host::PluginToolList>()
            .await
            .map(plugin_tool_list)
            .map_err(|error| PluginToolError::Unreachable(error.to_string()))
    }
}

#[async_trait]
impl PluginTools for SocketTools {
    async fn tools(&self) -> Result<PluginToolList, PluginToolError> {
        self.list(None, CALL_TIMEOUT).await
    }

    async fn call(
        &self,
        plugin: &PluginId,
        tool: &ToolName,
        call: PluginToolCall,
        tool_use: Option<&str>,
    ) -> Result<PluginToolResult, PluginToolError> {
        let name = kernel::plugins::tool_name(plugin, tool);
        let request = self
            .http
            .post(format!("http://plugins/tools/{name}"))
            .bearer_auth(self.bearer().await?)
            .timeout(CALL_TIMEOUT)
            .json(&plugin_tool_call(call));
        let request = match tool_use {
            Some(id) => request.header(TOOL_USE_HEADER, id),
            None => request,
        };
        let response = request
            .send()
            .await
            .map_err(|error| PluginToolError::Unreachable(error.to_string()))?;
        if response.status().is_success() {
            return response
                .json::<process::PluginToolResult>()
                .await
                .map(plugin_tool_result)
                .map_err(|error| PluginToolError::Unreachable(error.to_string()));
        }
        let status = response.status();
        let refused = response
            .json::<wire::gateway::GatewayError>()
            .await
            .map_err(|error| PluginToolError::Unreachable(error.to_string()))?;
        Err(PluginToolError::Failed {
            plugin: plugin.as_ref().to_owned(),
            tool: tool.as_ref().to_owned(),
            detail: format!("{status}: {}", refused.error),
        })
    }

    async fn changed(&self, seen: u64) -> Result<u64, PluginToolError> {
        loop {
            let list = self.list(Some(seen), LONG_POLL).await?;
            if list.version != seen {
                return Ok(list.version);
            }
        }
    }
}

fn plugin_tool_list(value: host::PluginToolList) -> plugins::PluginToolList {
    plugins::PluginToolList {
        version: value.version,
        tools: value.tools.into_iter().map(plugin_tool_entry).collect(),
    }
}

fn plugin_tool_entry(value: host::PluginToolEntry) -> plugins::PluginToolEntry {
    plugins::PluginToolEntry {
        plugin: value.plugin,
        tool: plugin_tool(value.tool),
    }
}

fn plugin_tool(value: manifest::PluginTool) -> plugins::PluginTool {
    plugins::PluginTool {
        name: value.name,
        title: value.title,
        description: value.description,
        input_schema: value.input_schema,
        hints: value.hints.into_iter().map(plugin_tool_hint).collect(),
    }
}

const fn plugin_tool_hint(value: manifest::PluginToolHint) -> plugins::PluginToolHint {
    match value {
        manifest::PluginToolHint::ReadOnly => plugins::PluginToolHint::ReadOnly,
        manifest::PluginToolHint::Destructive => plugins::PluginToolHint::Destructive,
        manifest::PluginToolHint::Idempotent => plugins::PluginToolHint::Idempotent,
        manifest::PluginToolHint::OpenWorld => plugins::PluginToolHint::OpenWorld,
    }
}

fn plugin_tool_result(value: process::PluginToolResult) -> plugins::PluginToolResult {
    plugins::PluginToolResult {
        text: value.text,
        is_error: value.is_error,
    }
}

fn plugin_tool_call(value: plugins::PluginToolCall) -> process::PluginToolCall {
    process::PluginToolCall {
        arguments: value.arguments,
        session_id: value.session_id,
    }
}
