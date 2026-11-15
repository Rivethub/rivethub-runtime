use std::sync::Arc;

use harness_acp::AcpHarnessAdapter;
use kernel::harness::ApprovalDecision;
use kernel::harness::Command;
use kernel::harness::HarnessAdapter;
use kernel::harness::PlanEntry;
use kernel::harness::PlanPriority;
use kernel::harness::PlanStatus;
use kernel::harness::SessionEvent;
use kernel::harness::SessionSpec;
use kernel::harness::ToolCallInfo;
use kernel::harness::ToolCallStatus;
use kernel::harness::ToolCallUpdate;
use kernel::harness::TurnInput;
use serde::Deserialize;
use serde::Serialize;
use tempfile::tempdir;
use wire::harness::HarnessId;
use wire::json::JsonObject;
use wire::session_id::SessionId;
use wire::stream::LlmUsage;
use wire::stream::ThinkingLevel;

#[cfg(test)]
mod protocol_support;
use protocol_support::{SESSION, collect_until_turn_end, launch, run, spec};

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct CommandInput {
    command: String,
}

fn expected_events(input: JsonObject) -> Vec<SessionEvent> {
    vec![
        SessionEvent::AgentMessageChunk {
            text: "hello".to_owned(),
        },
        SessionEvent::AgentThoughtChunk {
            text: "pondering".to_owned(),
        },
        SessionEvent::UserMessageChunk {
            text: "echo".to_owned(),
        },
        SessionEvent::ToolCall(ToolCallInfo {
            tool_call_id: "fake-tool-1".to_owned(),
            name: "run build".to_owned(),
            input,
        }),
        SessionEvent::ToolCallUpdate(ToolCallUpdate {
            tool_call_id: "fake-tool-1".to_owned(),
            status: ToolCallStatus::Completed,
            output: Some("build ok".to_owned()),
            title: None,
            input: None,
        }),
        SessionEvent::Plan(vec![PlanEntry {
            content: "measure the thing".to_owned(),
            priority: PlanPriority::High,
            status: PlanStatus::InProgress,
        }]),
        SessionEvent::AvailableCommands(vec![Command {
            name: "review".to_owned(),
            description: "review the change".to_owned(),
        }]),
        SessionEvent::CurrentMode {
            mode: "fast".to_owned(),
        },
        SessionEvent::TurnEnded,
    ]
}

#[test]
fn a_turn_streams_the_mapped_events_in_order() {
    let adapter = AcpHarnessAdapter::with_launch_table(launch(
        &[
            "text:hello",
            "thought:pondering",
            "user:echo",
            "tool:run build",
            "tool-update:fake-tool-1:completed",
            "plan",
            "commands",
            "mode:fast",
        ],
        &[],
    ));
    run(async {
        let cwd = tempdir().expect("cwd");
        let session = adapter
            .new_session(spec(cwd.path()))
            .await
            .expect("new session");
        assert_eq!(session.as_str(), "opencode:ses_fake0123456789abcdefgh");
        let mut events = adapter.events(&session).await.expect("events");
        adapter
            .prompt(&session, TurnInput::Text("do the thing".to_owned()))
            .await
            .expect("prompt");
        let collected = collect_until_turn_end(&mut events)
            .await
            .expect("events before turn end");
        let input: JsonObject = serde_json::from_value(
            serde_json::to_value(CommandInput {
                command: "ls -1".to_owned(),
            })
            .expect("input serializes"),
        )
        .expect("object input");
        assert_eq!(collected, expected_events(input));
    });
}

#[test]
fn the_prompt_result_usage_arrives_before_the_turn_end() {
    let adapter = AcpHarnessAdapter::with_launch_table(launch(&["text:hello"], &["--usage"]));
    run(async {
        let cwd = tempdir().expect("cwd");
        let session = adapter
            .new_session(spec(cwd.path()))
            .await
            .expect("new session");
        let mut events = adapter.events(&session).await.expect("events");
        adapter
            .prompt(&session, TurnInput::Text("do the thing".to_owned()))
            .await
            .expect("prompt");
        assert_eq!(
            collect_until_turn_end(&mut events)
                .await
                .expect("events before turn end"),
            vec![
                SessionEvent::AgentMessageChunk {
                    text: "hello".to_owned(),
                },
                SessionEvent::Usage {
                    usage: LlmUsage {
                        prompt_tokens: 23_248,
                        completion_tokens: 4,
                        reasoning_tokens: None,
                        cached_tokens: None,
                        cache_creation_tokens: Some(12_776),
                        cache_read_tokens: Some(10_470),
                    },
                },
                SessionEvent::TurnEnded,
            ]
        );
    });
}

#[test]
fn a_permission_request_is_answered_through_the_port() {
    let adapter = Arc::new(AcpHarnessAdapter::with_launch_table(launch(
        &["tool:run build", "permission"],
        &[],
    )));
    run(async {
        let cwd = tempdir().expect("cwd");
        let session = adapter
            .new_session(spec(cwd.path()))
            .await
            .expect("new session");
        let mut events = adapter.events(&session).await.expect("events");
        let prompt_task = tokio::spawn({
            let adapter = adapter.clone();
            let session = session.clone();
            async move {
                adapter
                    .prompt(&session, TurnInput::Text("go".to_owned()))
                    .await
            }
        });
        let mut seen = Vec::new();
        let request = loop {
            let event = events.recv().await.expect("event");
            let SessionEvent::Permission(request) = event else {
                seen.push(event);
                continue;
            };
            break request;
        };
        assert_eq!(seen.len(), 1);
        assert!(matches!(
            seen.first(),
            Some(SessionEvent::ToolCall(ToolCallInfo { name, .. })) if name == "run build"
        ));
        assert_eq!(request.name, "run the build");
        assert_eq!(request.tool_call_id.as_deref(), Some("fake-tool-1"));
        assert_eq!(request.reason, None);
        let input: CommandInput =
            serde_json::from_value(serde_json::to_value(&request.input).expect("input serializes"))
                .expect("input decodes");
        assert_eq!(input.command, "cargo build");
        adapter
            .answer_permission(&session, &request.request_id, ApprovalDecision::Allow)
            .await
            .expect("allow");
        prompt_task.await.expect("prompt task").expect("prompt ok");
        let rest = collect_until_turn_end(&mut events)
            .await
            .expect("events before turn end");
        assert_eq!(rest, vec![SessionEvent::TurnEnded]);
        let wrong = adapter
            .answer_permission(&session, "acp-permission-999", ApprovalDecision::Allow)
            .await
            .expect_err("unknown request");
        assert!(wrong.to_string().contains("acp-permission-999"));
    });
}

#[test]
fn a_denied_permission_ends_the_turn_normally() {
    let adapter = Arc::new(AcpHarnessAdapter::with_launch_table(launch(
        &["permission"],
        &[],
    )));
    run(async {
        let cwd = tempdir().expect("cwd");
        let session = adapter
            .new_session(spec(cwd.path()))
            .await
            .expect("new session");
        let mut events = adapter.events(&session).await.expect("events");
        let prompt_task = tokio::spawn({
            let adapter = adapter.clone();
            let session = session.clone();
            async move {
                adapter
                    .prompt(&session, TurnInput::Text("go".to_owned()))
                    .await
            }
        });
        let request = loop {
            let event = events.recv().await.expect("event");
            let SessionEvent::Permission(request) = event else {
                continue;
            };
            break request;
        };
        adapter
            .answer_permission(&session, &request.request_id, ApprovalDecision::Deny)
            .await
            .expect("deny");
        prompt_task.await.expect("prompt task").expect("prompt ok");
        let rest = collect_until_turn_end(&mut events)
            .await
            .expect("events before turn end");
        assert_eq!(rest, vec![SessionEvent::TurnEnded]);
    });
}

#[test]
fn an_unanswerable_decision_is_a_typed_error() {
    let adapter = Arc::new(AcpHarnessAdapter::with_launch_table(launch(
        &["permission"],
        &[],
    )));
    run(async {
        let cwd = tempdir().expect("cwd");
        let session = adapter
            .new_session(spec(cwd.path()))
            .await
            .expect("new session");
        let mut events = adapter.events(&session).await.expect("events");
        let prompt_task = tokio::spawn({
            let adapter = adapter.clone();
            let session = session.clone();
            async move {
                adapter
                    .prompt(&session, TurnInput::Text("go".to_owned()))
                    .await
            }
        });
        let request = loop {
            let event = events.recv().await.expect("event");
            let SessionEvent::Permission(request) = event else {
                continue;
            };
            break request;
        };
        let wrong = adapter
            .answer_permission(
                &session,
                &request.request_id,
                ApprovalDecision::AllowSession,
            )
            .await
            .expect_err("no allow-always option offered");
        assert!(wrong.to_string().contains("AllowSession"));
        adapter.cancel(&session).await.expect("cancel");
        prompt_task.await.expect("prompt task").expect("prompt ok");
    });
}

#[test]
fn cancel_ends_a_hanging_turn() {
    let adapter = Arc::new(AcpHarnessAdapter::with_launch_table(launch(
        &["text:partial"],
        &["--hang-until-cancel"],
    )));
    run(async {
        let cwd = tempdir().expect("cwd");
        let session = adapter
            .new_session(spec(cwd.path()))
            .await
            .expect("new session");
        let mut events = adapter.events(&session).await.expect("events");
        let prompt_task = tokio::spawn({
            let adapter = adapter.clone();
            let session = session.clone();
            async move {
                adapter
                    .prompt(&session, TurnInput::Text("go".to_owned()))
                    .await
            }
        });
        loop {
            let event = events.recv().await.expect("event");
            let SessionEvent::AgentMessageChunk { text } = event else {
                continue;
            };
            if text == "partial" {
                break;
            }
        }
        adapter.cancel(&session).await.expect("cancel");
        prompt_task.await.expect("prompt task").expect("prompt ok");
        let rest = collect_until_turn_end(&mut events)
            .await
            .expect("events before turn end");
        assert_eq!(rest, vec![SessionEvent::TurnEnded]);
    });
}

#[test]
fn a_crashed_agent_ends_the_stream() {
    let adapter = AcpHarnessAdapter::with_launch_table(launch(&[], &["--crash-on", "prompt"]));
    run(async {
        let cwd = tempdir().expect("cwd");
        let session = adapter
            .new_session(spec(cwd.path()))
            .await
            .expect("new session");
        let mut events = adapter.events(&session).await.expect("events");
        let error = adapter
            .prompt(&session, TurnInput::Text("go".to_owned()))
            .await
            .expect_err("agent crashed");
        assert!(error.to_string().contains("session/prompt"));
        let mut drained = Vec::new();
        while let Some(event) = events.recv().await {
            drained.push(event);
        }
        assert!(
            drained
                .iter()
                .any(|event| matches!(event, SessionEvent::Exited { .. })),
            "the exit is reported: {drained:?}"
        );
    });
}

#[test]
fn the_spec_model_and_effort_are_applied_at_new_session() {
    let adapter = AcpHarnessAdapter::with_launch_table(launch(&[], &[]));
    run(async {
        let cwd = tempdir().expect("cwd");
        let full = SessionSpec {
            session: SessionId::new(HarnessId::Opencode, SESSION),
            harness: HarnessId::Opencode,
            model: Some("grok-5".to_owned()),
            effort: Some(ThinkingLevel::Low),
            working_dir: Some(cwd.path().to_path_buf()),
        };
        let session = adapter.new_session(full).await.expect("new session");
        let mut events = adapter.events(&session).await.expect("events");
        let mut collected = Vec::new();
        for _ in 0..2 {
            collected.push(events.recv().await.expect("config event"));
        }
        assert_eq!(
            collected,
            vec![
                SessionEvent::AgentMessageChunk {
                    text: "model=grok-5".to_owned(),
                },
                SessionEvent::AgentMessageChunk {
                    text: "thought_level=low".to_owned(),
                },
            ]
        );
    });
}

#[test]
fn an_effort_is_left_to_a_harness_that_offers_no_effort_switch() {
    let adapter = AcpHarnessAdapter::with_launch_table(launch(&[], &["--effort-option", "absent"]));
    run(async {
        let cwd = tempdir().expect("cwd");
        let session = adapter
            .new_session(SessionSpec {
                session: SessionId::new(HarnessId::Opencode, SESSION),
                harness: HarnessId::Opencode,
                model: Some("grok-5".to_owned()),
                effort: Some(ThinkingLevel::High),
                working_dir: Some(cwd.path().to_path_buf()),
            })
            .await
            .expect("new session without an effort switch");
        let mut events = adapter.events(&session).await.expect("events");
        assert_eq!(
            events.recv().await.expect("config event"),
            SessionEvent::AgentMessageChunk {
                text: "model=grok-5".to_owned(),
            }
        );
    });
}

#[test]
fn port_refusals_are_typed() {
    let adapter = AcpHarnessAdapter::with_launch_table(launch(&[], &[]));
    run(async {
        let cwd = tempdir().expect("cwd");
        let unknown = SessionId::new(HarnessId::Opencode, "ses_absent000000000000000001");
        let error = adapter
            .prompt(&unknown, TurnInput::Text("go".to_owned()))
            .await
            .expect_err("unknown session");
        assert!(matches!(
            error,
            kernel::harness::HarnessAdapterError::UnknownSession(_)
        ));
        let error = adapter.events(&unknown).await.expect_err("unknown session");
        assert!(matches!(
            error,
            kernel::harness::HarnessAdapterError::UnknownSession(_)
        ));
        let session = adapter
            .new_session(spec(cwd.path()))
            .await
            .expect("new session");
        let first = adapter.events(&session).await.expect("events");
        let error = adapter.events(&session).await.expect_err("stream taken");
        assert!(matches!(
            error,
            kernel::harness::HarnessAdapterError::EventStreamTaken(_)
        ));
        drop(first);
        let mismatched = SessionSpec {
            session: SessionId::new(
                HarnessId::KimiCode,
                "session_00000000-0000-4000-8000-000000000000",
            ),
            harness: HarnessId::Opencode,
            model: None,
            effort: None,
            working_dir: Some(cwd.path().to_path_buf()),
        };
        let error = adapter
            .new_session(mismatched)
            .await
            .expect_err("harness mismatch");
        assert!(matches!(
            error,
            kernel::harness::HarnessAdapterError::InvalidSession(_)
        ));
    });
}

#[test]
fn raw_array_tool_input_has_no_event_form() {
    let adapter = AcpHarnessAdapter::with_launch_table(launch(&["tool-raw-array"], &[]));
    run(async {
        let cwd = tempdir().expect("cwd");
        let session = adapter
            .new_session(spec(cwd.path()))
            .await
            .expect("new session");
        let mut events = adapter.events(&session).await.expect("events");
        adapter
            .prompt(&session, TurnInput::Text("go".to_owned()))
            .await
            .expect("prompt");
        let collected = collect_until_turn_end(&mut events)
            .await
            .expect("events before turn end");
        assert_eq!(collected, vec![SessionEvent::TurnEnded]);
    });
}
