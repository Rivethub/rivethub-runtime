use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use agent_client_protocol::Agent;
use agent_client_protocol::Client;
use agent_client_protocol::ConnectionTo;
use agent_client_protocol::Responder;
use agent_client_protocol::Stdio;
use agent_client_protocol::schema::v1 as acp;
use clap::Parser;
use parking_lot::Mutex;
use tokio::sync::Notify;

mod script;

use script::Selection;
use script::Step;

#[derive(Parser)]
#[command(disable_help_flag = true, disable_version_flag = true)]
struct FakeArgs {
    #[arg(long)]
    session_id: String,
    #[arg(long)]
    steps: Vec<String>,
    #[arg(long)]
    crash_on: Option<CrashPoint>,
    #[arg(long)]
    hang_until_cancel: bool,
    #[arg(long)]
    list_sessions: bool,
    #[arg(long)]
    usage: bool,
    #[arg(long)]
    record_new: Option<std::path::PathBuf>,
    #[arg(long)]
    record_delete: Option<std::path::PathBuf>,
    #[arg(long, default_value = "offered")]
    effort_option: EffortOption,
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum EffortOption {
    Offered,
    Absent,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum CrashPoint {
    Prompt,
}

struct AgentState {
    session_id: String,
    steps: Vec<Step>,
    crash_on: Option<CrashPoint>,
    hang_until_cancel: bool,
    list_sessions: bool,
    usage: bool,
    record_new: Option<std::path::PathBuf>,
    record_delete: Option<std::path::PathBuf>,
    effort_option: EffortOption,
    cancelled: AtomicBool,
    cancel_notify: Notify,
    crashed: Notify,
    selection: Mutex<Selection>,
}

impl AgentState {
    async fn crash(&self) -> std::convert::Infallible {
        self.crashed.notify_one();
        std::future::pending().await
    }
}

fn prepare(args: FakeArgs) -> Option<AgentState> {
    let mut steps = Vec::with_capacity(args.steps.len());
    for raw in &args.steps {
        let Ok(parsed) = script::parse_step(raw) else {
            return None;
        };
        steps.push(parsed);
    }
    Some(AgentState {
        session_id: args.session_id,
        steps,
        crash_on: args.crash_on,
        hang_until_cancel: args.hang_until_cancel,
        list_sessions: args.list_sessions,
        usage: args.usage,
        record_new: args.record_new,
        record_delete: args.record_delete,
        effort_option: args.effort_option,
        cancelled: AtomicBool::new(false),
        cancel_notify: Notify::new(),
        crashed: Notify::new(),
        selection: Mutex::new(Selection::initial()),
    })
}

fn session_id(state: &AgentState) -> acp::SessionId {
    acp::SessionId::new(state.session_id.clone())
}

fn current_options(state: &AgentState) -> Vec<acp::SessionConfigOption> {
    let mut options = script::fixture_options(&state.selection.lock());
    if state.effort_option == EffortOption::Absent {
        options.retain(|option| {
            option.category != Some(acp::SessionConfigOptionCategory::ThoughtLevel)
        });
    }
    options
}

fn on_initialize(
    state: &AgentState,
    request: &acp::InitializeRequest,
    responder: Responder<acp::InitializeResponse>,
) -> Result<(), acp::Error> {
    responder.respond(
        acp::InitializeResponse::new(request.protocol_version).agent_capabilities(
            acp::AgentCapabilities::new().session_capabilities(
                acp::SessionCapabilities::new()
                    .list(state.list_sessions.then(acp::SessionListCapabilities::new))
                    .delete(
                        state
                            .record_delete
                            .is_some()
                            .then(acp::SessionDeleteCapabilities::new),
                    ),
            ),
        ),
    )
}

fn record(path: &std::path::Path, line: &str) -> Result<(), acp::Error> {
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| writeln!(file, "{line}"))
        .map_err(|error| acp::Error::internal_error().data(error.to_string()))
}

fn on_new_session(
    state: &AgentState,
    request: &acp::NewSessionRequest,
    responder: Responder<acp::NewSessionResponse>,
) -> Result<(), acp::Error> {
    if let Some(path) = &state.record_new {
        record(path, &request.cwd.display().to_string())?;
    }
    responder.respond(
        acp::NewSessionResponse::new(session_id(state)).config_options(current_options(state)),
    )
}

fn on_set_config(
    state: &AgentState,
    request: acp::SetSessionConfigOptionRequest,
    responder: Responder<acp::SetSessionConfigOptionResponse>,
    connection: &ConnectionTo<Client>,
) -> Result<(), acp::Error> {
    let echo = {
        let mut selection = state.selection.lock();
        match script::apply_selection(&mut selection, &request) {
            Ok(echo) => echo,
            Err(error) => return responder.respond_with_error(error),
        }
    };
    connection.send_notification(acp::SessionNotification::new(
        request.session_id,
        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::from(
            echo,
        ))),
    ))?;
    responder.respond(acp::SetSessionConfigOptionResponse::new(current_options(
        state,
    )))
}

fn spawn_prompt(
    state: Arc<AgentState>,
    request: acp::PromptRequest,
    responder: Responder<acp::PromptResponse>,
    connection: &ConnectionTo<Client>,
) -> Result<(), acp::Error> {
    let task_connection = connection.clone();
    connection.spawn(async move {
        let session_id = request.session_id;
        let executed = execute_prompt(&state, &task_connection, &session_id).await;
        match executed {
            Ok(stop) => responder.respond(ended(stop, state.usage)),
            Err(error) => responder.respond_with_error(error),
        }
    })
}

fn ended(stop: acp::StopReason, usage: bool) -> acp::PromptResponse {
    let response = acp::PromptResponse::new(stop);
    if usage {
        response.usage(
            acp::Usage::new(23_252, 2, 4)
                .cached_read_tokens(10_470)
                .cached_write_tokens(12_776),
        )
    } else {
        response
    }
}

async fn execute_prompt(
    state: &AgentState,
    connection: &ConnectionTo<Client>,
    session_id: &acp::SessionId,
) -> Result<acp::StopReason, acp::Error> {
    if matches!(state.crash_on, Some(CrashPoint::Prompt)) {
        match state.crash().await {}
    }
    for step in &state.steps {
        if let Some(stop) = script::run_step(connection, session_id, step).await? {
            return Ok(stop);
        }
    }
    if state.hang_until_cancel {
        wait_until_cancelled(state).await;
        return Ok(acp::StopReason::Cancelled);
    }
    Ok(acp::StopReason::EndTurn)
}

async fn wait_until_cancelled(state: &AgentState) {
    loop {
        let notified = state.cancel_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if state.cancelled.load(Ordering::SeqCst) {
            return;
        }
        notified.await;
    }
}

fn on_cancel(state: &AgentState) {
    state.cancelled.store(true, Ordering::SeqCst);
    state.cancel_notify.notify_one();
}

async fn serve(state: Arc<AgentState>) -> Result<(), acp::Error> {
    let initialize_state = Arc::clone(&state);
    let new_state = Arc::clone(&state);
    let config_state = Arc::clone(&state);
    let prompt_state = Arc::clone(&state);
    let cancel_state = Arc::clone(&state);
    let delete_state = Arc::clone(&state);
    Agent
        .builder()
        .name("fake-acp-agent")
        .on_receive_request(
            async move |request: acp::InitializeRequest, responder, _connection| {
                on_initialize(&initialize_state, &request, responder)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::NewSessionRequest, responder, _connection| {
                on_new_session(&new_state, &request, responder)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::SetSessionConfigOptionRequest, responder, connection| {
                on_set_config(&config_state, request, responder, &connection)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::PromptRequest, responder, connection| {
                spawn_prompt(Arc::clone(&prompt_state), request, responder, &connection)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::DeleteSessionRequest, responder, _connection| {
                if let Some(path) = &delete_state.record_delete {
                    record(path, &request.session_id.0)?;
                }
                responder.respond(acp::DeleteSessionResponse::new())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |_notification: acp::CancelNotification, _connection| {
                on_cancel(&cancel_state);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_to(Stdio::new())
        .await
}

fn main() -> std::process::ExitCode {
    let args = FakeArgs::parse();
    let Some(state) = prepare(args) else {
        return std::process::ExitCode::FAILURE;
    };
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        return std::process::ExitCode::FAILURE;
    };
    let state = Arc::new(state);
    let watched = Arc::clone(&state);
    let served = runtime.block_on(async {
        tokio::select! {
            served = serve(state) => served.is_ok(),
            () = watched.crashed.notified() => false,
        }
    });
    runtime.shutdown_background();
    if served {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}
