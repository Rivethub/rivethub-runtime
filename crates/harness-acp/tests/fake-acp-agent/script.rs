use agent_client_protocol::Client;
use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::v1::schema as acp;

pub(crate) struct Selection {
    pub model: String,
    pub effort: String,
}

impl Selection {
    pub(crate) fn initial() -> Self {
        Self {
            model: "grok-4.7".to_owned(),
            effort: "high".to_owned(),
        }
    }
}

enum Selector {
    Model,
    Effort,
}

struct Chosen {
    selector: Selector,
    value: String,
}

pub(crate) enum Step {
    Text(String),
    Thought(String),
    User(String),
    Tool(String),
    RawArrayTool,
    ToolUpdate(String, acp::ToolCallStatus),
    Plan,
    Commands,
    Mode(String),
    Permission,
}

pub(crate) fn parse_step(spec: &str) -> Result<Step, String> {
    let (kind, rest) = match spec.split_once(':') {
        Some(parts) => parts,
        None => (spec, ""),
    };
    match kind {
        "text" => Ok(Step::Text(rest.to_owned())),
        "thought" => Ok(Step::Thought(rest.to_owned())),
        "user" => Ok(Step::User(rest.to_owned())),
        "tool" => Ok(Step::Tool(rest.to_owned())),
        "tool-raw-array" => Ok(Step::RawArrayTool),
        "tool-update" => parse_tool_update(spec, rest),
        "plan" => Ok(Step::Plan),
        "commands" => Ok(Step::Commands),
        "mode" => Ok(Step::Mode(rest.to_owned())),
        "permission" => Ok(Step::Permission),
        other => Err(format!("unknown fake agent step {other}")),
    }
}

fn parse_tool_update(spec: &str, rest: &str) -> Result<Step, String> {
    let (id, status) = rest
        .split_once(':')
        .ok_or_else(|| format!("tool-update needs id:status, got {spec}"))?;
    let status = match status {
        "pending" => acp::ToolCallStatus::Pending,
        "in-progress" => acp::ToolCallStatus::InProgress,
        "failed" => acp::ToolCallStatus::Failed,
        "completed" => acp::ToolCallStatus::Completed,
        other => return Err(format!("unknown tool-update status {other}")),
    };
    Ok(Step::ToolUpdate(id.to_owned(), status))
}

pub(crate) fn fixture_options(selection: &Selection) -> Vec<acp::SessionConfigOption> {
    vec![
        acp::SessionConfigOption::select(
            "model",
            "Model",
            selection.model.clone(),
            vec![
                acp::SessionConfigSelectOption::new("grok-5", "Grok 5"),
                acp::SessionConfigSelectOption::new("grok-4.7", "Grok 4.7 (recommended)"),
                acp::SessionConfigSelectOption::new("grok-4.5", "Grok 4.5"),
            ],
        )
        .category(acp::SessionConfigOptionCategory::Model),
        acp::SessionConfigOption::select(
            "reasoning_effort",
            "Reasoning effort",
            selection.effort.clone(),
            choices(&["off", "low", "medium", "high", "xhigh"]),
        )
        .category(acp::SessionConfigOptionCategory::ThoughtLevel),
    ]
}

fn choices(ids: &[&str]) -> Vec<acp::SessionConfigSelectOption> {
    ids.iter()
        .map(|id| acp::SessionConfigSelectOption::new((*id).to_owned(), (*id).to_owned()))
        .collect()
}

pub(crate) fn apply_selection(
    selection: &mut Selection,
    request: &acp::SetSessionConfigOptionRequest,
) -> Result<String, agent_client_protocol::Error> {
    let chosen = choose(selection, request)?;
    match chosen.selector {
        Selector::Model => selection.model.clone_from(&chosen.value),
        Selector::Effort => selection.effort.clone_from(&chosen.value),
    }
    Ok(echo(&chosen))
}

fn choose(
    selection: &Selection,
    request: &acp::SetSessionConfigOptionRequest,
) -> Result<Chosen, agent_client_protocol::Error> {
    let value = match request.value.as_value_id() {
        Some(id) => id.0.to_string(),
        None => {
            return Err(
                agent_client_protocol::Error::invalid_params().data("config value is not an id")
            );
        }
    };
    let options = fixture_options(selection);
    let found = options.iter().find(|option| option.id == request.config_id);
    let Some(found) = found else {
        return Err(
            agent_client_protocol::Error::invalid_params().data("config option is not offered")
        );
    };
    let offered = offered_ids(found)?;
    if !offered.iter().any(|id| id.0.as_ref() == value) {
        return Err(agent_client_protocol::Error::invalid_params()
            .data(format!("config value {value} is not offered")));
    }
    let selector = match &found.category {
        Some(acp::SessionConfigOptionCategory::Model) => Selector::Model,
        Some(acp::SessionConfigOptionCategory::ThoughtLevel) => Selector::Effort,
        Some(
            acp::SessionConfigOptionCategory::Mode
            | acp::SessionConfigOptionCategory::ModelConfig
            | acp::SessionConfigOptionCategory::Other(_)
            | _,
        )
        | None => {
            return Err(agent_client_protocol::Error::invalid_params()
                .data("config option has no selector category"));
        }
    };
    Ok(Chosen { selector, value })
}

fn offered_ids(
    option: &acp::SessionConfigOption,
) -> Result<Vec<acp::SessionConfigValueId>, agent_client_protocol::Error> {
    match &option.kind {
        acp::SessionConfigKind::Select(select) => Ok(select_ids(&select.options)),
        acp::SessionConfigKind::Boolean(_) | _ => {
            Err(agent_client_protocol::Error::invalid_params()
                .data("config option is not a selector"))
        }
    }
}

fn select_ids(options: &acp::SessionConfigSelectOptions) -> Vec<acp::SessionConfigValueId> {
    match options {
        acp::SessionConfigSelectOptions::Ungrouped(items) => {
            items.iter().map(|item| item.value.clone()).collect()
        }
        acp::SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| group.options.iter().map(|item| item.value.clone()))
            .collect(),
        _ => Vec::new(),
    }
}

fn echo(chosen: &Chosen) -> String {
    match chosen.selector {
        Selector::Model => format!("model={}", chosen.value),
        Selector::Effort => format!("thought_level={}", chosen.value),
    }
}

pub(crate) async fn run_step(
    connection: &ConnectionTo<Client>,
    session_id: &acp::SessionId,
    step: &Step,
) -> Result<Option<acp::StopReason>, agent_client_protocol::Error> {
    match step {
        Step::Text(text) => emit_text(connection, session_id, text, Chunk::Agent)?,
        Step::Thought(text) => emit_text(connection, session_id, text, Chunk::Thought)?,
        Step::User(text) => emit_text(connection, session_id, text, Chunk::User)?,
        Step::Tool(title) => emit(connection, session_id, tool_call(title)?)?,
        Step::RawArrayTool => emit(connection, session_id, raw_array_tool()?)?,
        Step::ToolUpdate(id, status) => emit(connection, session_id, tool_update(id, *status))?,
        Step::Plan => emit(connection, session_id, plan_update())?,
        Step::Commands => emit(connection, session_id, commands_update())?,
        Step::Mode(mode) => emit(
            connection,
            session_id,
            acp::SessionUpdate::CurrentModeUpdate(acp::CurrentModeUpdate::new(mode.clone())),
        )?,
        Step::Permission => return permission_step(connection, session_id).await,
    }
    Ok(None)
}

#[derive(Clone, Copy)]
pub(crate) enum Chunk {
    Agent,
    Thought,
    User,
}

pub(crate) fn emit_text(
    connection: &ConnectionTo<Client>,
    session_id: &acp::SessionId,
    text: &str,
    chunk: Chunk,
) -> Result<(), agent_client_protocol::Error> {
    let content = acp::ContentChunk::new(acp::ContentBlock::from(text.to_owned()));
    let update = match chunk {
        Chunk::Agent => acp::SessionUpdate::AgentMessageChunk(content),
        Chunk::Thought => acp::SessionUpdate::AgentThoughtChunk(content),
        Chunk::User => acp::SessionUpdate::UserMessageChunk(content),
    };
    emit(connection, session_id, update)
}

fn emit(
    connection: &ConnectionTo<Client>,
    session_id: &acp::SessionId,
    update: acp::SessionUpdate,
) -> Result<(), agent_client_protocol::Error> {
    connection.send_notification(acp::SessionNotification::new(session_id.clone(), update))
}

fn tool_call(title: &str) -> Result<acp::SessionUpdate, agent_client_protocol::Error> {
    let input = serde_json::to_value(CommandInput { command: "ls -1" })
        .map_err(|error| agent_client_protocol::Error::internal_error().data(error.to_string()))?;
    Ok(acp::SessionUpdate::ToolCall(
        acp::ToolCall::new("fake-tool-1", title.to_owned())
            .kind(acp::ToolKind::Execute)
            .status(acp::ToolCallStatus::InProgress)
            .raw_input(input),
    ))
}

fn raw_array_tool() -> Result<acp::SessionUpdate, agent_client_protocol::Error> {
    let input = serde_json::to_value(RawArray(&["not", "an", "object"]))
        .map_err(|error| agent_client_protocol::Error::internal_error().data(error.to_string()))?;
    Ok(acp::SessionUpdate::ToolCall(
        acp::ToolCall::new("fake-tool-1", "raw")
            .kind(acp::ToolKind::Other)
            .status(acp::ToolCallStatus::InProgress)
            .raw_input(input),
    ))
}

fn tool_update(id: &str, status: acp::ToolCallStatus) -> acp::SessionUpdate {
    let mut fields = acp::ToolCallUpdateFields::new().status(status);
    if matches!(status, acp::ToolCallStatus::Completed) {
        fields = fields.content(vec![acp::ToolCallContent::from("build ok")]);
    }
    acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(id.to_owned(), fields))
}

fn plan_update() -> acp::SessionUpdate {
    acp::SessionUpdate::Plan(acp::Plan::new(vec![acp::PlanEntry::new(
        "measure the thing",
        acp::PlanEntryPriority::High,
        acp::PlanEntryStatus::InProgress,
    )]))
}

fn commands_update() -> acp::SessionUpdate {
    acp::SessionUpdate::AvailableCommandsUpdate(acp::AvailableCommandsUpdate::new(vec![
        acp::AvailableCommand::new("review", "review the change"),
    ]))
}

async fn permission_step(
    connection: &ConnectionTo<Client>,
    session_id: &acp::SessionId,
) -> Result<Option<acp::StopReason>, agent_client_protocol::Error> {
    let request = acp::RequestPermissionRequest::new(
        session_id.clone(),
        acp::ToolCallUpdate::new(
            "fake-tool-1",
            acp::ToolCallUpdateFields::new()
                .status(acp::ToolCallStatus::Pending)
                .title("run the build".to_owned())
                .raw_input(
                    serde_json::to_value(CommandInput {
                        command: "cargo build",
                    })
                    .map_err(|error| {
                        agent_client_protocol::Error::internal_error().data(error.to_string())
                    })?,
                ),
        ),
        vec![
            acp::PermissionOption::new("allow", "Allow", acp::PermissionOptionKind::AllowOnce),
            acp::PermissionOption::new("reject", "Reject", acp::PermissionOptionKind::RejectOnce),
        ],
    );
    let response = connection.send_request(request).block_task().await?;
    match response.outcome {
        acp::RequestPermissionOutcome::Cancelled => Ok(Some(acp::StopReason::Cancelled)),
        acp::RequestPermissionOutcome::Selected(_) => Ok(None),
        _ => {
            Err(agent_client_protocol::Error::internal_error()
                .data("unrecognized permission outcome"))
        }
    }
}

#[derive(serde::Serialize)]
struct CommandInput<'a> {
    command: &'a str,
}

#[derive(serde::Serialize)]
struct RawArray<'a>(&'a [&'a str]);
