#![cfg(test)]
use crate::session_support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use gateway::tasks::LocalAgent;
use gateway::tasks::Tasks;
use gateway::term::Delegate;
use kernel::harness::{PermissionRequest, SessionEvent};
use kernel::transcript::{
    ListLimit, SessionLister, SessionRenameError, StoreSession, ToolOutputRead, TranscriptRead,
    TranscriptReaderError, TranscriptWindow,
};
use session_support::harness::Harness;
use session_support::{Fixture, http, user};
use testkit::TaskMemory;
use testkit::wiki_routes::NoParents;
use tokio_util::sync::CancellationToken;
use wire::harness::{HarnessId, HarnessSessionCreateRequest, HarnessSessionSummary};
use wire::task::{
    AcceptanceCriterion, CriterionKind, TaskCreateRequest, TaskResponse, TaskStatus,
    TaskWaitTimeoutResponse,
};

struct Running {
    fixture: Fixture,
    harness: Arc<Harness>,
    stop: CancellationToken,
    home: tempfile::TempDir,
}

async fn running() -> Running {
    running_with(None).await
}

async fn running_with(lister: Option<Arc<dyn SessionLister>>) -> Running {
    let harness = Arc::new(Harness::default());
    let home = tempfile::tempdir().expect("agent workspace");
    let delegates = BTreeMap::from([(
        "worker".to_owned(),
        Delegate {
            harness: HarnessId::ClaudeCode,
            model: Some("opus".to_owned()),
            cwd: Some(home.path().to_path_buf()),
            permission_mode: Some("bypassPermissions".to_owned()),
        },
    )]);
    let memory = TaskMemory::new();
    let tasks = Tasks::new(
        Arc::new(memory.clone()),
        Arc::new(memory.clone()),
        Arc::new(memory),
        "golden-node".to_owned(),
    )
    .with_agents(BTreeMap::from([(
        "worker".to_owned(),
        LocalAgent::default(),
    )]));
    let fixture = Fixture::with_delegates(
        Some(harness.clone()),
        gateway::sessions::SessionRegistry::new(Duration::from_secs(1)),
        lister,
        &[],
        delegates,
        Some(Arc::new(tasks)),
    )
    .await;
    let stop = CancellationToken::new();
    tokio::spawn(gateway::tasks::serve(fixture.gateway.clone(), stop.clone()));
    Running {
        fixture,
        harness,
        stop,
        home,
    }
}

async fn submit(running: &Running, parent: Option<&str>) -> String {
    let body = TaskCreateRequest {
        goal: Some("ship it".to_owned()),
        agent_id: Some("worker".to_owned()),
        acceptance_criteria: Some(vec![AcceptanceCriterion {
            id: "1".to_owned(),
            description: "tests pass".to_owned(),
            kind: CriterionKind::Manual,
            check: None,
        }]),
        parent_session_id: parent.map(str::to_owned),
        ..TaskCreateRequest::default()
    };
    let (status, reply) = http(
        &running.fixture.gateway,
        "POST",
        "/api/tasks",
        serde_json::to_vec(&body).expect("body"),
        None,
    )
    .await;
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&reply));
    let created: TaskResponse = serde_json::from_slice(&reply).expect("task");
    created.task.id
}

async fn wait(running: &Running, id: &str, millis: u64) -> (u16, Vec<u8>) {
    http(
        &running.fixture.gateway,
        "GET",
        &format!("/api/tasks/{id}/wait?timeoutMs={millis}"),
        Vec::new(),
        None,
    )
    .await
}

fn first_turn(id: &str) -> String {
    format!(
        "prompt:ship it\n\nAcceptance criteria:\n- tests pass\n\nReply with a short summary of what you did and the result; it goes back to the session that asked (task {id})."
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delegated_task_runs_as_a_session_that_stays_open_and_answers_approvals_by_its_mode() {
    let running = running().await;
    let id = submit(&running, Some("claude-code:parent")).await;
    running.harness.traced(&first_turn(&id)).await;
    let spec = running.harness.specs.lock().first().cloned().expect("spec");
    assert_eq!(
        (
            spec.harness,
            spec.model.as_deref(),
            spec.working_dir.as_deref()
        ),
        (
            HarnessId::ClaudeCode,
            Some("opus"),
            Some(running.home.path())
        )
    );
    running
        .harness
        .emit(
            &spec.session,
            SessionEvent::Permission(PermissionRequest {
                request_id: "ask-1".into(),
                tool_call_id: None,
                name: "Bash".into(),
                input: wire::JsonObject::default(),
                reason: None,
            }),
        )
        .await;
    running.harness.traced("answer:ask-1:Allow").await;
    running.harness.finish.add_permits(1);
    let (status, reply) = wait(&running, &id, 5_000).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&reply));
    let done: TaskResponse = serde_json::from_slice(&reply).expect("task");
    assert_eq!(done.task.status, TaskStatus::Completed);
    assert_eq!(
        done.task.result.map(|result| result.summary).as_deref(),
        Some("answer")
    );
    assert_eq!(
        done.task.child_session_id.as_deref(),
        Some(spec.session.as_str())
    );
    assert!(
        !running
            .harness
            .trace
            .borrow()
            .contains(&"release".to_owned())
    );
    let listed = running
        .fixture
        .service
        .harness_sessions(&user("owner"))
        .sessions;
    let child: &HarnessSessionSummary = listed.first().expect("the child stays listed");
    assert_eq!(
        child
            .parent_session_id
            .as_ref()
            .map(wire::session_id::SessionId::as_str),
        Some("claude-code:parent")
    );
    assert_eq!(child.task_id.as_deref(), Some(id.as_str()));
    running.stop.cancel();
    running.fixture.gateway.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wait_that_ends_leaves_the_task_running_and_only_a_kill_stops_it() {
    let running = running().await;
    let id = submit(&running, None).await;
    running.harness.traced(&first_turn(&id)).await;
    let (status, reply) = wait(&running, &id, 50).await;
    assert_eq!(status, 504);
    let left: TaskWaitTimeoutResponse = serde_json::from_slice(&reply).expect("left");
    assert_eq!(left.task.map(|task| task.status), Some(TaskStatus::Running));
    let (status, _) = http(
        &running.fixture.gateway,
        "POST",
        &format!("/api/tasks/{id}/kill"),
        b"{}".to_vec(),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let (_, reply) = wait(&running, &id, 5_000).await;
    let done: TaskResponse = serde_json::from_slice(&reply).expect("task");
    assert_eq!(done.task.status, TaskStatus::Killed);
    running.stop.cancel();
    running.fixture.gateway.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_result_that_arrives_after_the_wait_comes_back_to_the_parent_as_a_new_turn() {
    let running = running().await;
    let (status, reply) = http(
        &running.fixture.gateway,
        "POST",
        "/api/harness-sessions",
        serde_json::to_vec(&HarnessSessionCreateRequest {
            harness: Some(HarnessId::ClaudeCode),
            model: None,
            cwd: None,
            agent_id: None,
            topic_id: None,
            prompt: None,
        })
        .expect("body"),
        None,
    )
    .await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&reply));
    let parent: HarnessSessionSummary = serde_json::from_slice(&reply).expect("parent");
    let id = submit(&running, Some(parent.session_id.as_str())).await;
    running.harness.traced(&first_turn(&id)).await;
    let (status, _) = wait(&running, &id, 50).await;
    assert_eq!(status, 504);
    running.harness.finish.add_permits(2);
    let late = format!(
        "prompt:Task {id} finished (completed) after your delegate_task call stopped waiting.\n\nanswer\n\nChild session "
    );
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut trace = running.harness.trace.subscribe();
        loop {
            if trace
                .borrow_and_update()
                .iter()
                .any(|entry| entry.starts_with(&late))
            {
                return;
            }
            trace.changed().await.expect("trace open");
        }
    })
    .await
    .expect("the late result reaches the parent");
    running.stop.cancel();
    running.fixture.gateway.close().await;
}

const TERMINAL_PARENT: &str = "00000000-0000-4000-8000-0000000000a1";

#[derive(Debug)]
struct PlanMode;

#[async_trait]
impl SessionLister for PlanMode {
    async fn list(&self, harnesses: &[HarnessId], limit: ListLimit) -> Vec<StoreSession> {
        NoParents.list(harnesses, limit).await
    }

    async fn describe(
        &self,
        harness: HarnessId,
        native: &str,
        parent: Option<&str>,
    ) -> Result<Option<StoreSession>, TranscriptReaderError> {
        NoParents.describe(harness, native, parent).await
    }

    async fn store_cwd(
        &self,
        harness: HarnessId,
        native: &str,
    ) -> Result<Option<String>, TranscriptReaderError> {
        NoParents.store_cwd(harness, native).await
    }

    async fn permission_mode(
        &self,
        harness: HarnessId,
        native: &str,
    ) -> Result<Option<String>, TranscriptReaderError> {
        Ok(
            (harness == HarnessId::ClaudeCode && native == TERMINAL_PARENT)
                .then(|| "plan".to_owned()),
        )
    }

    async fn transcript(
        &self,
        harness: HarnessId,
        native: &str,
        window: TranscriptWindow,
    ) -> Result<TranscriptRead, TranscriptReaderError> {
        NoParents.transcript(harness, native, window).await
    }

    async fn rename(
        &self,
        harness: HarnessId,
        native: &str,
        title: &str,
    ) -> Result<StoreSession, SessionRenameError> {
        NoParents.rename(harness, native, title).await
    }

    async fn tool_output(
        &self,
        harness: HarnessId,
        native: &str,
        tool_call_id: &str,
    ) -> Result<ToolOutputRead, TranscriptReaderError> {
        NoParents.tool_output(harness, native, tool_call_id).await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_terminal_parent_hands_its_child_the_mode_its_harness_records() {
    let running = running_with(Some(Arc::new(PlanMode))).await;
    running
        .fixture
        .service
        .spawn(
            &user("owner"),
            wire::gateway::term::TermSpawnRequest {
                session: Some(TERMINAL_PARENT.to_owned()),
                command: Some("claude".to_owned()),
                model: None,
                effort: None,
                resume: None,
                agent_id: None,
                force: None,
                cols: None,
                rows: None,
                topic_id: None,
            },
        )
        .await
        .expect("terminal parent");
    let id = submit(&running, Some(&format!("claude-code:{TERMINAL_PARENT}"))).await;
    running.harness.traced(&first_turn(&id)).await;
    let trace = running.harness.trace.borrow().clone();
    assert!(
        trace.contains(&r#"mode:Mode("plan")"#.to_owned()),
        "{trace:?}"
    );
    running.stop.cancel();
    running.fixture.gateway.close().await;
}
