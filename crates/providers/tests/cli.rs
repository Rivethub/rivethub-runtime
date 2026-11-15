#![cfg(test)]

mod cli_replay;
mod cli_support;

use std::sync::Arc;
use std::time::Duration;

use cli_support::{Action, Fault, Harness, Plan, TestResult, action, items, provider, request};
use config::provider::CliProvider;
use kernel::harness::ApprovalDecision;
use kernel::harness::{PermissionRequest, SessionEvent, ToolCallInfo, TurnInput};
use kernel::model::LlmResponseKind;
use kernel::model::{ModelChunk, ModelProvider};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use wire::stream::{LlmUsage, ThinkingLevel};

fn text(text: &str) -> SessionEvent {
    SessionEvent::AgentMessageChunk { text: text.into() }
}

fn usage() -> LlmUsage {
    LlmUsage {
        prompt_tokens: 25,
        completion_tokens: 5,
        reasoning_tokens: Some(22),
        cached_tokens: Some(10),
        cache_creation_tokens: None,
        cache_read_tokens: None,
    }
}

#[derive(serde::Deserialize)]
struct HeadlessInput {
    messages: Vec<wire::message::Message>,
    system_prompt: String,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_turn_preserves_context_and_maps_events_then_releases() -> TestResult {
    let directory = tempfile::tempdir()?;
    let arguments = serde_json::from_str(r#"{"text":"hi","n":1.0}"#)?;
    let (harness, mut actions) = Harness::new(vec![Plan {
        events: vec![
            SessionEvent::AgentThoughtChunk {
                text: "考える 🦀".into(),
            },
            SessionEvent::ToolCall(ToolCallInfo {
                tool_call_id: "call-1".into(),
                name: "echo".into(),
                input: arguments,
            }),
            text("Hello 🦀"),
            SessionEvent::Usage { usage: usage() },
        ],
        finish: true,
    }]);
    let harness = Arc::new(harness);
    let config = CliProvider {
        cwd: Some(directory.path().into()),
        effort: Some(config::Effort::Low),
        system_prompt: Some("Preserve quoted input".into()),
        ..CliProvider::default()
    };
    let provider = provider(Arc::clone(&harness), config, &CancellationToken::new());
    let mut input = request();
    input.thinking = Some(ThinkingLevel::High);
    let stream = provider.stream(input.clone()).await?;
    let chunks = items(stream)
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        chunks,
        [
            ModelChunk::Reasoning {
                delta: "考える 🦀".into()
            },
            ModelChunk::Text {
                delta: "Hello 🦀".into()
            },
            ModelChunk::Usage { usage: usage() },
            ModelChunk::Done,
        ]
    );
    let Action::New(spec, canonical) = action(&mut actions).await else {
        panic!("new session")
    };
    assert_ne!(spec.session, canonical);
    assert_eq!(spec.model, Some(input.model));
    assert_eq!(spec.effort, Some(ThinkingLevel::High));
    assert_eq!(spec.working_dir.as_deref(), Some(directory.path()));
    let Action::Prompt(session, TurnInput::Text(prompt)) = action(&mut actions).await else {
        panic!("prompt")
    };
    assert_eq!(session, canonical);
    let prompt: HeadlessInput = serde_json::from_str(&prompt)?;
    assert_eq!(prompt.messages, input.messages);
    assert_eq!(prompt.system_prompt, "Preserve quoted input");
    assert!(matches!(action(&mut actions).await, Action::Release(id) if id == canonical));
    provider.shutdown().await;
    assert!(harness.is_empty());
    Ok(())
}

#[tokio::test]
async fn permissions_are_refused_without_blocking_prompt_events() -> TestResult {
    let (harness, mut actions) = Harness::new(vec![Plan {
        events: vec![
            SessionEvent::Permission(PermissionRequest {
                request_id: "permission-1".into(),
                tool_call_id: None,
                name: "shell".into(),
                input: serde_json::from_str(r#"{"command":"echo 'hello' > result.txt"}"#)?,
                reason: None,
            }),
            text("Refused"),
        ],
        finish: true,
    }]);
    let harness = Arc::new(harness);
    let provider = provider(
        Arc::clone(&harness),
        CliProvider::default(),
        &CancellationToken::new(),
    );
    let response = tokio::time::timeout(Duration::from_secs(5), provider.chat(request())).await??;
    assert_eq!(response.content.as_deref(), Some("Refused"));
    assert_eq!(response.r#type, LlmResponseKind::Text);
    assert!(matches!(action(&mut actions).await, Action::New(..)));
    assert!(matches!(action(&mut actions).await, Action::Prompt(..)));
    assert!(matches!(
        action(&mut actions).await,
        Action::Permission(ApprovalDecision::Deny)
    ));
    assert!(matches!(action(&mut actions).await, Action::Release(_)));
    provider.shutdown().await;
    assert!(harness.is_empty());
    Ok(())
}

#[tokio::test]
async fn bypass_permissions_without_a_mode_option_allows_the_request() -> TestResult {
    let (mut harness, mut actions) = Harness::new(vec![Plan {
        events: vec![
            SessionEvent::Permission(PermissionRequest {
                request_id: "permission-1".into(),
                tool_call_id: None,
                name: "shell".into(),
                input: serde_json::from_str(r#"{"command":"true"}"#)?,
                reason: None,
            }),
            text("Allowed"),
        ],
        finish: true,
    }]);
    harness.fault = Some(Fault::NoModeOption);
    let harness = Arc::new(harness);
    let provider = provider(
        Arc::clone(&harness),
        CliProvider {
            permission_mode: Some("bypassPermissions".into()),
            ..CliProvider::default()
        },
        &CancellationToken::new(),
    );
    let response = tokio::time::timeout(Duration::from_secs(5), provider.chat(request())).await??;
    assert_eq!(response.content.as_deref(), Some("Allowed"));
    assert!(matches!(action(&mut actions).await, Action::New(..)));
    assert!(matches!(action(&mut actions).await, Action::Prompt(..)));
    assert!(matches!(
        action(&mut actions).await,
        Action::Permission(ApprovalDecision::Allow)
    ));
    assert!(matches!(action(&mut actions).await, Action::Release(_)));
    provider.shutdown().await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn permission_timeout_cancels_and_releases_a_stalled_answer() -> TestResult {
    let (mut harness, actions) = Harness::new(vec![Plan {
        events: vec![SessionEvent::Permission(PermissionRequest {
            request_id: "stalled".into(),
            tool_call_id: None,
            name: "shell".into(),
            input: wire::JsonObject::default(),
            reason: None,
        })],
        finish: false,
    }]);
    harness.fault = Some(Fault::PermissionHang);
    let harness = Arc::new(harness);
    let provider = provider(
        Arc::clone(&harness),
        CliProvider {
            permission_timeout_ms: Some(100),
            ..CliProvider::default()
        },
        &CancellationToken::new(),
    );
    let error = tokio::time::timeout(Duration::from_secs(1), provider.chat(request()))
        .await?
        .expect_err("permission deadline");
    assert!(error.to_string().contains("timed out"));
    provider.shutdown().await;
    assert!(harness.is_empty());
    drop(actions);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_consumer_cancels_only_its_session() -> TestResult {
    let (harness, mut actions) = Harness::new(vec![
        Plan {
            events: vec![text("first")],
            finish: false,
        },
        Plan {
            events: vec![text("second")],
            finish: true,
        },
    ]);
    let harness = Arc::new(harness);
    let provider = provider(
        Arc::clone(&harness),
        CliProvider::default(),
        &CancellationToken::new(),
    );
    let mut first = provider.stream(request()).await?;
    assert!(
        tokio::time::timeout(Duration::from_secs(5), first.recv())
            .await?
            .is_some()
    );
    let Action::New(_, first_id) = action(&mut actions).await else {
        panic!("first session")
    };
    assert!(matches!(action(&mut actions).await, Action::Prompt(..)));
    let second = tokio::time::timeout(Duration::from_secs(5), provider.chat(request())).await??;
    assert_eq!(second.content.as_deref(), Some("second"));
    let Action::New(spec, second_id) = action(&mut actions).await else {
        panic!("second session")
    };
    assert_ne!(first_id, second_id);
    assert_ne!(first_id, spec.session);
    assert!(matches!(action(&mut actions).await, Action::Prompt(..)));
    assert!(matches!(action(&mut actions).await, Action::Release(id) if id == second_id));
    drop(first);
    assert!(matches!(action(&mut actions).await, Action::Cancel(id) if id == first_id));
    assert!(matches!(action(&mut actions).await, Action::Release(id) if id == first_id));
    provider.shutdown().await;
    assert!(harness.is_empty());
    Ok(())
}

#[tokio::test]
async fn cancellation_during_startup_releases_the_created_session() -> TestResult {
    let gate = Arc::new(Notify::new());
    let (mut harness, mut actions) = Harness::new(vec![Plan {
        events: Vec::new(),
        finish: false,
    }]);
    harness.start_gate = Some(Arc::clone(&gate));
    let harness = Arc::new(harness);
    let cancel = CancellationToken::new();
    let provider = provider(Arc::clone(&harness), CliProvider::default(), &cancel);
    let receiver = provider.stream(request()).await?;
    tokio::time::timeout(Duration::from_secs(5), harness.started.notified()).await?;
    cancel.cancel();
    gate.notify_one();
    let chunks = items(receiver).await;
    assert_eq!(chunks.len(), 1);
    assert!(chunks.first().expect("cancellation error").is_err());
    assert!(matches!(action(&mut actions).await, Action::New(..)));
    assert!(matches!(action(&mut actions).await, Action::Cancel(_)));
    assert!(matches!(action(&mut actions).await, Action::Release(_)));
    provider.shutdown().await;
    assert!(provider.stream(request()).await.is_err());
    assert!(harness.is_empty());
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn deadline_cancels_and_releases_the_turn() -> TestResult {
    let (harness, mut actions) = Harness::new(vec![Plan {
        events: vec![text("started")],
        finish: false,
    }]);
    let harness = Arc::new(harness);
    let provider = provider(
        Arc::clone(&harness),
        CliProvider {
            timeout_ms: Some(100),
            ..CliProvider::default()
        },
        &CancellationToken::new(),
    );
    let mut stream = provider.stream(request()).await?;
    assert!(
        tokio::time::timeout(Duration::from_secs(1), stream.recv())
            .await?
            .is_some()
    );
    tokio::time::advance(Duration::from_millis(101)).await;
    let chunks = items(stream).await;
    assert_eq!(chunks.len(), 1);
    assert!(
        chunks
            .first()
            .expect("timeout error")
            .as_ref()
            .is_err_and(|error| error.to_string().contains("timed out"))
    );
    assert!(matches!(action(&mut actions).await, Action::New(..)));
    assert!(matches!(action(&mut actions).await, Action::Prompt(..)));
    assert!(matches!(action(&mut actions).await, Action::Cancel(_)));
    assert!(matches!(action(&mut actions).await, Action::Release(_)));
    provider.shutdown().await;
    assert!(harness.is_empty());
    Ok(())
}

#[tokio::test]
async fn adapter_errors_remain_errors_and_release_is_always_attempted() -> TestResult {
    for fault in [
        Fault::New,
        Fault::Events,
        Fault::Prompt,
        Fault::Release,
        Fault::Permission,
    ] {
        let events = vec![SessionEvent::Permission(PermissionRequest {
            request_id: "p".into(),
            tool_call_id: None,
            name: "shell".into(),
            input: wire::JsonObject::default(),
            reason: None,
        })];
        let (mut harness, actions) = Harness::new(vec![Plan {
            events,
            finish: true,
        }]);
        harness.fault = Some(fault);
        let harness = Arc::new(harness);
        let provider = provider(
            Arc::clone(&harness),
            CliProvider::default(),
            &CancellationToken::new(),
        );
        let error = tokio::time::timeout(Duration::from_secs(5), provider.chat(request()))
            .await?
            .expect_err("adapter failure");
        assert!(
            error.to_string().contains(&format!("{fault:?} failure")),
            "{error}"
        );
        provider.shutdown().await;
        assert!(harness.is_empty());
        drop(actions);
    }
    Ok(())
}

#[tokio::test]
async fn premature_exit_is_not_a_successful_turn() -> TestResult {
    let (harness, actions) = Harness::new(vec![Plan {
        events: vec![text("partial"), SessionEvent::Exited { code: Some(9) }],
        finish: false,
    }]);
    let harness = Arc::new(harness);
    let provider = provider(
        Arc::clone(&harness),
        CliProvider::default(),
        &CancellationToken::new(),
    );
    let error = tokio::time::timeout(Duration::from_secs(5), provider.chat(request()))
        .await?
        .expect_err("early exit");
    assert!(error.to_string().contains("Some(9)"));
    provider.shutdown().await;
    assert!(harness.is_empty());
    drop(actions);
    Ok(())
}
