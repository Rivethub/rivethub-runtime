use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use capture::hook::ClaudeHook;
use capture::writer::{CaptureTransport, PostError};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use wire::capture::{CaptureBatchRequest, CaptureResult};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SESSION: &str = "33333333-3333-3333-3333-333333333333";
const HELD: &str = "remember where the cabin key is";

#[derive(Clone, Default)]
struct Posts {
    bodies: Arc<Mutex<Vec<String>>>,
}

impl Posts {
    fn held_keys(&self) -> Result<Vec<String>, serde_json::Error> {
        let prefix = format!("claude-code:{SESSION}:hook:");
        let mut keys = Vec::new();
        for body in self.bodies.lock().iter() {
            let batch: CaptureBatchRequest = serde_json::from_str(body)?;
            keys.extend(
                batch
                    .messages
                    .iter()
                    .filter_map(|message| message.event_id.strip_prefix(&prefix))
                    .map(str::to_owned),
            );
        }
        keys.sort();
        Ok(keys)
    }

    fn accept(&self, body: String) -> CaptureResult {
        self.bodies.lock().push(body);
        CaptureResult {
            ok: true,
            conversation_id: "conv-1".to_owned(),
            inserted: 1,
            skipped: 0,
        }
    }
}

#[async_trait]
impl CaptureTransport for Posts {
    async fn post(&self, body: String) -> Result<CaptureResult, PostError> {
        Ok(self.accept(body))
    }
}

#[derive(Clone)]
struct HoldsWhilePosting {
    posts: Posts,
    home: PathBuf,
}

#[async_trait]
impl CaptureTransport for HoldsWhilePosting {
    async fn post(&self, body: String) -> Result<CaptureResult, PostError> {
        let beside =
            hook(&self.home, &self.home.join("spool"), Posts::default()).map_err(|error| {
                PostError::Network {
                    message: error.to_string(),
                }
            })?;
        let run = beside.run(&held("held while posting"), now(), "late").await;
        if !run.stderr.is_empty() {
            return Err(PostError::Network {
                message: run.stderr,
            });
        }
        Ok(self.posts.accept(body))
    }
}

fn hook<T: CaptureTransport>(
    home: &Path,
    spool: &Path,
    transport: T,
) -> Result<ClaudeHook<T>, config::ConfigError> {
    let env = config::Environment::from_vars([("HOME".to_owned(), home.display().to_string())]);
    let writer = capture::writer::CaptureWriter::new(
        transport,
        capture::spool::CaptureSpool::new(spool.to_path_buf()),
        std::num::NonZeroUsize::new(768 * 1024),
        None,
    );
    Ok(ClaudeHook::new(writer, &config::Paths::new(&env)?))
}

fn now() -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(1_791_656_243, 0).unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

fn held(prompt: &str) -> String {
    format!(
        r#"{{"session_id":"{SESSION}","hook_event_name":"UserPromptSubmit","prompt":"{prompt}"}}"#
    )
}

fn transcript(home: &Path, extra: &str) -> std::io::Result<PathBuf> {
    let store = home.join("store").join("projects").join("-work-demo");
    std::fs::create_dir_all(&store)?;
    let target = store.join(format!("{SESSION}.jsonl"));
    let mut text =
        std::fs::read_to_string(testkit::recording("capture/claude-hook/transcript.jsonl"))?;
    text.push_str(extra);
    std::fs::write(&target, text)?;
    Ok(target)
}

fn stop(transcript: &Path) -> String {
    format!(
        r#"{{"session_id":"{SESSION}","hook_event_name":"Stop","transcript_path":"{}"}}"#,
        transcript.display()
    )
}

fn pending_dir(home: &Path) -> PathBuf {
    home.join(".rivethub").join("claude-pending")
}

fn pending_files(home: &Path) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(pending_dir(home))? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
            && let Some(name) = path.file_name()
        {
            names.push(name.to_string_lossy().into_owned());
        }
    }
    Ok(names)
}

#[tokio::test]
async fn a_batch_that_fails_to_build_keeps_its_held_prompt() -> TestResult {
    let home = tempfile::tempdir()?;
    let posts = Posts::default();
    let capture = hook(home.path(), &home.path().join("spool"), posts.clone())?;
    assert_eq!(capture.run(&held(HELD), now(), "k1").await.stderr, "");
    let broken = transcript(
        home.path(),
        &format!(
            r#"{{"type":"user","sessionId":"{SESSION}","uuid":"u9","timestamp":"not a time","message":{{"role":"user","content":"and the boat"}}}}
"#
        ),
    )?;
    let failed = capture.run(&stop(&broken), now(), "k2").await;
    assert_ne!(
        failed.stderr, "",
        "a row with a bad timestamp fails the batch"
    );
    assert!(posts.bodies.lock().is_empty());
    let fixed = transcript(home.path(), "")?;
    assert_eq!(capture.run(&stop(&fixed), now(), "k3").await.stderr, "");
    assert_eq!(
        posts.held_keys()?,
        ["k1"],
        "the held prompt survives the failed run"
    );
    assert_eq!(pending_files(home.path())?, Vec::<String>::new());
    Ok(())
}

#[tokio::test]
async fn a_batch_that_never_reached_the_spool_keeps_its_held_prompt() -> TestResult {
    let home = tempfile::tempdir()?;
    let blocked = home.path().join("not-a-directory");
    std::fs::write(&blocked, "")?;
    let posts = Posts::default();
    let unspooled = hook(home.path(), &blocked, posts.clone())?;
    assert_eq!(unspooled.run(&held(HELD), now(), "k1").await.stderr, "");
    let path = transcript(home.path(), "")?;
    let failed = unspooled.run(&stop(&path), now(), "k2").await;
    assert_ne!(failed.stderr, "", "the spool cannot be written");
    assert!(posts.bodies.lock().is_empty());
    let capture = hook(home.path(), &home.path().join("spool"), posts.clone())?;
    assert_eq!(capture.run(&stop(&path), now(), "k3").await.stderr, "");
    assert_eq!(posts.held_keys()?, ["k1"]);
    Ok(())
}

#[tokio::test]
async fn a_claim_left_by_an_interrupted_capture_is_sent_next_time() -> TestResult {
    let home = tempfile::tempdir()?;
    let dir = pending_dir(home.path());
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join(format!("{SESSION}.claimed-0.jsonl")),
        format!(r#"{{"key":"k0","prompt":"{HELD}","at":"2026-10-10T12:00:09Z"}}"#) + "\n",
    )?;
    let posts = Posts::default();
    let capture = hook(home.path(), &home.path().join("spool"), posts.clone())?;
    let path = transcript(home.path(), "")?;
    assert_eq!(capture.run(&stop(&path), now(), "k3").await.stderr, "");
    assert_eq!(posts.held_keys()?, ["k0"]);
    assert_eq!(pending_files(home.path())?, Vec::<String>::new());
    Ok(())
}

#[tokio::test]
async fn a_prompt_held_while_a_capture_posts_waits_for_the_next() -> TestResult {
    let home = tempfile::tempdir()?;
    let posts = Posts::default();
    let racing = hook(
        home.path(),
        &home.path().join("spool"),
        HoldsWhilePosting {
            posts: posts.clone(),
            home: home.path().to_path_buf(),
        },
    )?;
    assert_eq!(racing.run(&held(HELD), now(), "k1").await.stderr, "");
    let path = transcript(home.path(), "")?;
    assert_eq!(racing.run(&stop(&path), now(), "k2").await.stderr, "");
    assert_eq!(posts.held_keys()?, ["k1"]);
    assert_eq!(pending_files(home.path())?, [format!("{SESSION}.jsonl")]);
    let capture = hook(home.path(), &home.path().join("spool"), posts.clone())?;
    assert_eq!(capture.run(&stop(&path), now(), "k3").await.stderr, "");
    assert_eq!(posts.held_keys()?, ["k1", "late"]);
    Ok(())
}
