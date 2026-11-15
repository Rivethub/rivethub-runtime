use std::sync::Arc;
use std::time::Duration;

use config::provider::CliProvider;
use kernel::harness::SessionEvent;
use kernel::model::{ModelChunk, ModelProvider};
use tokio_util::sync::CancellationToken;

use crate::cli_support::{
    Action, Fault, Harness, Plan, TestResult, action, items, provider, request,
};

#[tokio::test]
async fn eof_and_prompt_error_after_turn_end_cannot_report_success() -> TestResult {
    for fault in [Fault::EventsClosed, Fault::PromptAfterEvents] {
        let (mut harness, actions) = Harness::new(vec![Plan {
            events: Vec::new(),
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
            .expect_err("incomplete turn");
        assert!(
            error.to_string().contains("before the turn completed")
                || error.to_string().contains("PromptAfterEvents failure")
        );
        provider.shutdown().await;
        assert!(harness.is_empty());
        drop(actions);
    }
    Ok(())
}

#[tokio::test]
async fn provider_drop_cancels_and_releases_without_a_runtime_task_leak() -> TestResult {
    let (harness, mut actions) = Harness::new(vec![Plan {
        events: vec![SessionEvent::AgentMessageChunk {
            text: "started".into(),
        }],
        finish: false,
    }]);
    let harness = Arc::new(harness);
    let provider = provider(
        Arc::clone(&harness),
        CliProvider::default(),
        &CancellationToken::new(),
    );
    let mut stream = provider.stream(request()).await?;
    assert!(
        tokio::time::timeout(Duration::from_secs(5), stream.recv())
            .await?
            .is_some()
    );
    drop(provider);
    let result = items(stream).await;
    assert!(result.iter().all(Result::is_err));
    assert!(matches!(action(&mut actions).await, Action::New(..)));
    assert!(matches!(action(&mut actions).await, Action::Prompt(..)));
    assert!(matches!(action(&mut actions).await, Action::Cancel(_)));
    assert!(matches!(action(&mut actions).await, Action::Release(_)));
    assert!(harness.is_empty());
    Ok(())
}

#[tokio::test]
async fn cleanup_failure_preserves_the_original_turn_error() -> TestResult {
    let (mut harness, actions) = Harness::new(vec![Plan {
        events: vec![SessionEvent::Exited { code: Some(7) }],
        finish: false,
    }]);
    harness.fault = Some(Fault::Cancel);
    let harness = Arc::new(harness);
    let provider = provider(
        Arc::clone(&harness),
        CliProvider::default(),
        &CancellationToken::new(),
    );
    let chunks = items(provider.stream(request()).await?).await;
    assert!(
        !chunks
            .iter()
            .any(|item| matches!(item, Ok(ModelChunk::Done)))
    );
    let error = chunks
        .into_iter()
        .next()
        .expect("error item")
        .expect_err("cleanup failure");
    assert!(error.to_string().contains("Some(7)"));
    assert!(error.to_string().contains("Cancel failure"));
    provider.shutdown().await;
    assert!(harness.is_empty());
    drop(actions);
    Ok(())
}
