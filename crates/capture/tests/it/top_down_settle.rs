use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::sync::Arc;

use async_trait::async_trait;
use capture::spool::CaptureSpool;
use capture::writer::{CaptureTransport, CaptureWriter, PostError};
use chrono::{DateTime, TimeDelta, Utc};
use parking_lot::Mutex;
use wire::capture::{CaptureBatchRequest, CaptureResult};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Clone, Default)]
struct Transport {
    posts: Arc<Mutex<Vec<CaptureBatchRequest>>>,
    failures: Arc<Mutex<VecDeque<PostError>>>,
}

#[async_trait]
impl CaptureTransport for Transport {
    async fn post(&self, body: String) -> Result<CaptureResult, PostError> {
        let batch: CaptureBatchRequest =
            serde_json::from_str(&body).map_err(|error| PostError::Network {
                message: error.to_string(),
            })?;
        self.posts.lock().push(batch);
        if let Some(error) = self.failures.lock().pop_front() {
            return Err(error);
        }
        Ok(CaptureResult {
            ok: true,
            conversation_id: "settlement-id".to_owned(),
            inserted: 1,
            skipped: 0,
        })
    }
}

fn end_batch() -> Result<CaptureBatchRequest, serde_json::Error> {
    serde_json::from_value(serde_json::json!({
        "session_key":"claude-code:settle-test",
        "agent":"rivet",
        "channel":"claude-code",
        "finalize":true,
        "settings": {
            "source":"claude-code-hook",
            "last_event":"SessionEnd",
            "last_ingest_at":"2026-10-10T12:00:00Z"
        },
        "messages": (0..3).map(|n| serde_json::json!({
            "event_id":n.to_string(), "role":"user", "content":"sample ".repeat(70)
        })).collect::<Vec<_>>(),
    }))
}

fn has_end(batch: &CaptureBatchRequest) -> bool {
    batch
        .session_observation()
        .and_then(|observation| observation.end_key)
        .is_some()
}

fn network() -> PostError {
    PostError::Network {
        message: "fixture offline".to_owned(),
    }
}

#[tokio::test]
async fn immediate_end_waits_and_preserves_legacy_finalize_after_client_rejection() -> TestResult {
    for reject in [false, true] {
        let dir = tempfile::tempdir()?;
        let transport = Transport::default();
        if reject {
            transport
                .failures
                .lock()
                .push_back(PostError::Client { status: 400 });
        }
        let writer = CaptureWriter::new(
            transport.clone(),
            CaptureSpool::new(dir.path().to_owned()),
            NonZeroUsize::new(950),
            None,
        );
        let result = writer
            .write(&end_batch()?, "2026-10-10T12:00:00Z".parse()?)
            .await;
        assert_eq!(result.is_err(), reject);
        let posts = transport.posts.lock();
        assert_eq!(posts.len(), 3);
        assert_eq!(
            posts.iter().map(has_end).collect::<Vec<_>>(),
            [false, false, !reject]
        );
        assert_eq!(posts[2].finalize, Some(true));
        assert_eq!(posts[2].messages[0].event_id, "2");
    }
    Ok(())
}

#[tokio::test]
async fn replay_end_waits_for_backoff_or_ages_out_after_a_dead_predecessor() -> TestResult {
    for reject in [false, true] {
        let dir = tempfile::tempdir()?;
        let transport = Transport::default();
        transport.failures.lock().push_back(network());
        let writer = CaptureWriter::new(
            transport.clone(),
            CaptureSpool::new(dir.path().to_owned()),
            NonZeroUsize::new(950),
            None,
        );
        let now: DateTime<Utc> = "2026-10-10T12:00:00Z".parse()?;
        assert!(writer.write(&end_batch()?, now).await.is_err());
        assert_eq!(transport.posts.lock().len(), 1);
        let replay = CaptureWriter::new(
            transport.clone(),
            CaptureSpool::new(dir.path().to_owned()),
            None,
            None,
        );
        if reject {
            transport
                .failures
                .lock()
                .push_back(PostError::Client { status: 400 });
            replay.replay(now).await?;
        } else {
            transport.failures.lock().push_back(network());
            replay.replay(now).await?;
            replay.replay(now + TimeDelta::seconds(1)).await?;
            assert!(
                transport
                    .posts
                    .lock()
                    .iter()
                    .all(|batch| batch.finalize.is_none())
            );
            replay.replay(now + TimeDelta::seconds(31)).await?;
        }
        let posts = transport.posts.lock();
        let final_chunk = posts.last().ok_or("final chunk")?;
        assert_eq!(final_chunk.finalize, Some(true));
        assert_eq!(has_end(final_chunk), !reject);
        assert!(posts[..posts.len() - 1].iter().all(|batch| !has_end(batch)));
    }
    Ok(())
}

#[tokio::test]
async fn claude_clear_forwards_the_previous_session_end() -> TestResult {
    use capture::hook::ClaudeHook;
    let dir = tempfile::tempdir()?;
    let env =
        config::Environment::from_vars([("HOME".to_owned(), dir.path().display().to_string())]);
    let transport = Transport::default();
    let hook = ClaudeHook::new(
        CaptureWriter::new(
            transport.clone(),
            CaptureSpool::new(dir.path().join("spool")),
            None,
            None,
        ),
        &config::Paths::new(&env)?,
    );
    let old = "bd823506-5d8b-4c3b-9892-0b69a47ab9a4";
    let new = "2e06bd25-fe4d-40ce-bc87-d1d35ff8fda8";
    let time = "2026-10-10T12:00:00Z".parse()?;
    let end = serde_json::json!({
        "session_id":old, "hook_event_name":"SessionEnd", "reason":"clear", "event_id":"native-end"
    });
    let start = serde_json::json!({
        "session_id":new, "hook_event_name":"SessionStart", "source":"clear"
    });
    assert_eq!(hook.run(&end.to_string(), time, "fixture").await.stderr, "");
    assert_eq!(
        hook.run(&start.to_string(), time, "fixture").await.stderr,
        ""
    );
    let posts = transport.posts.lock();
    let [notification] = posts.as_slice() else {
        return Err("expected only the previous session's end".into());
    };
    assert_eq!(notification.session_key, format!("claude-code:{old}"));
    let key = notification
        .session_observation()
        .and_then(|observation| observation.end_key)
        .ok_or("end marker")?;
    assert!(key.ends_with("native-end"));
    Ok(())
}

#[tokio::test]
async fn codex_empty_end_is_distinct_from_turn_boundaries_and_imports() -> TestResult {
    use capture::hook::store::{StoreHarness, StoreHook};
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(dir.path().join(".codex/sessions"))?;
    let env =
        config::Environment::from_vars([("HOME".to_owned(), dir.path().display().to_string())]);
    let transport = Transport::default();
    let hook = StoreHook::new(
        StoreHarness::Codex,
        CaptureWriter::new(
            transport.clone(),
            CaptureSpool::new(dir.path().join("spool")),
            None,
            None,
        ),
        &capture::listing::store_roots(&env),
        &config::Paths::new(&env)?,
    )
    .ok_or("hook")?;
    let time = "2026-10-10T12:00:00Z".parse()?;
    let end = r#"{"session_id":"invented","hook_event_name":"SessionEnd","reason":"clear"}"#;
    assert_eq!(hook.run(end, time).await.stderr, "");
    let posts = transport.posts.lock();
    let [notification] = posts.as_slice() else {
        return Err("expected one notification".into());
    };
    assert!(notification.messages.is_empty());
    assert!(has_end(notification));
    for (key, source, event) in [
        ("dsh:invented", "dsh-hook", "turn/end"),
        ("opencode:invented", "opencode-hook", "session.idle"),
        ("import:invented", "codex-hook", "SessionEnd"),
        ("claude-code:invented", "claude-code-hook", "SubagentStop"),
    ] {
        let mut unrelated = end_batch()?;
        unrelated.session_key = key.to_owned();
        unrelated.settings = Some(serde_json::from_value(serde_json::json!({
            "source":source, "last_event":event, "last_ingest_at":"2026-10-10T12:00:00Z",
        }))?);
        assert!(!has_end(&unrelated));
    }
    let mut harvested_child = end_batch()?;
    let settings = harvested_child
        .settings
        .as_mut()
        .ok_or("observed settings")?;
    settings.insert("parent_session_id", "claude-code:parent")?;
    assert!(!has_end(&harvested_child));
    assert_eq!(harvested_child.finalize, Some(true));
    Ok(())
}
