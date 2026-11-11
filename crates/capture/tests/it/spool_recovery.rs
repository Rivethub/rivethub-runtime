use std::num::NonZeroUsize;
use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use capture::spool::CaptureSpool;
use capture::writer::CaptureWriter;
use capture::writer::PostError;
use chrono::DateTime;
use chrono::Utc;
use parking_lot::Mutex;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureResult;

fn accepted() -> CaptureResult {
    CaptureResult {
        ok: true,
        conversation_id: "c-1".to_owned(),
        inserted: 1,
        skipped: 0,
    }
}

#[derive(Debug, Clone, Copy)]
enum Verdict {
    Accept,
    Reject,
    Unavailable,
}

#[derive(Default)]
struct RuleTransport {
    rules: Vec<(&'static str, Verdict)>,
    posts: Mutex<Vec<String>>,
}

impl RuleTransport {
    fn with(rules: Vec<(&'static str, Verdict)>) -> Self {
        Self {
            rules,
            posts: Mutex::new(Vec::new()),
        }
    }

    fn verdict(&self, body: &str) -> Verdict {
        self.rules
            .iter()
            .find(|(marker, _)| body.contains(marker))
            .map_or(Verdict::Accept, |(_, verdict)| *verdict)
    }

    fn posted(&self) -> Vec<String> {
        self.posts.lock().clone()
    }
}

#[async_trait]
impl capture::writer::CaptureTransport for &RuleTransport {
    async fn post(&self, body: String) -> Result<CaptureResult, PostError> {
        let verdict = self.verdict(&body);
        self.posts.lock().push(body);
        match verdict {
            Verdict::Accept => Ok(accepted()),
            Verdict::Reject => Err(PostError::Client { status: 422 }),
            Verdict::Unavailable => Err(PostError::Server { status: 500 }),
        }
    }
}

fn now_at(text: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
    text.parse()
}

fn batch(
    session: &str,
    messages: usize,
) -> Result<CaptureBatchRequest, capture::batch::BatchError> {
    let messages: Vec<String> = (0..messages)
        .map(|index| {
            format!(
                r#"{{"event_id":"e{index}","role":"user","content":"{}"}}"#,
                "m".repeat(400)
            )
        })
        .collect();
    capture::batch::parse_batch(&format!(
        r#"{{"session_key":"{session}","agent":"a","messages":[{}]}}"#,
        messages.join(",")
    ))
}

fn bodies(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if entry.file_type()?.is_file() {
                    names.push(entry.path());
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    names.sort();
    names.iter().map(std::fs::read_to_string).collect()
}

#[tokio::test]
async fn a_failing_spooled_batch_does_not_block_new_captures()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let spool = CaptureSpool::new(dir.path().join("spool"));
    let now = now_at("2026-10-10T12:00:00Z")?;
    spool.spool(r#"{"session_key":"stuck"}"#, now).await?;
    let transport = RuleTransport::with(vec![("stuck", Verdict::Unavailable)]);
    let writer = CaptureWriter::new(&transport, spool, None, None);
    let totals = writer.write(&batch("fresh", 1)?, now).await?;
    assert_eq!(totals.inserted, 1);
    let later = now + Duration::from_secs(5);
    writer.write(&batch("next", 1)?, later).await?;
    let posted = transport.posted();
    assert_eq!(
        posted.iter().filter(|body| body.contains("stuck")).count(),
        1
    );
    assert!(posted.iter().any(|body| body.contains("fresh")));
    assert!(posted.iter().any(|body| body.contains("next")));
    assert_eq!(bodies(&dir.path().join("spool"))?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn replay_moves_past_a_batch_waiting_out_its_backoff()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let spool = CaptureSpool::new(dir.path().to_path_buf());
    let now = now_at("2026-10-10T12:00:00Z")?;
    spool.spool(r#"{"session_key":"stuck"}"#, now).await?;
    spool
        .spool(r#"{"session_key":"later"}"#, now + Duration::from_millis(5))
        .await?;
    let transport = RuleTransport::with(vec![("stuck", Verdict::Unavailable)]);
    let handle = &transport;
    let post = |body: String| capture::writer::CaptureTransport::post(&handle, body);
    spool.replay(50, now, post).await?;
    assert_eq!(transport.posted().len(), 1);
    spool
        .replay(50, now + Duration::from_secs(10), post)
        .await?;
    assert_eq!(bodies(dir.path())?, vec![r#"{"session_key":"stuck"}"#]);
    spool
        .replay(50, now + Duration::from_secs(31), post)
        .await?;
    let stuck_posts = transport
        .posted()
        .iter()
        .filter(|body| body.contains("stuck"))
        .count();
    assert_eq!(stuck_posts, 2);
    Ok(())
}

#[tokio::test]
async fn a_batch_that_keeps_failing_is_dead_lettered() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let spool = CaptureSpool::new(dir.path().to_path_buf());
    let mut now = now_at("2026-10-10T12:00:00Z")?;
    spool.spool(r#"{"session_key":"stuck"}"#, now).await?;
    let transport = RuleTransport::with(vec![("stuck", Verdict::Unavailable)]);
    let handle = &transport;
    let post = |body: String| capture::writer::CaptureTransport::post(&handle, body);
    for _ in 0..capture::spool::MAX_TRANSIENT_ATTEMPTS {
        spool.replay(50, now, post).await?;
        now += Duration::from_secs(7200);
    }
    let attempts = usize::try_from(capture::spool::MAX_TRANSIENT_ATTEMPTS)?;
    assert_eq!(transport.posted().len(), attempts);
    assert_eq!(bodies(dir.path())?.len(), 0);
    assert_eq!(
        bodies(&spool.dead_dir())?,
        vec![r#"{"session_key":"stuck"}"#]
    );
    spool.replay(50, now, post).await?;
    assert_eq!(transport.posted().len(), attempts);
    Ok(())
}

#[tokio::test]
async fn the_spool_warns_once_per_threshold_crossing_and_never_drops_a_batch()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let spool = CaptureSpool::new(dir.path().to_path_buf());
    let now = now_at("2026-10-10T12:00:00Z")?;
    let mut paths = Vec::new();
    for index in 0..1000_u64 {
        paths.push(spool.spool("b", now + Duration::from_millis(index)).await?);
    }
    assert_eq!(spool.crossed_threshold().await?, None);
    paths.push(spool.spool("b", now + Duration::from_millis(1000)).await?);
    let crossed = spool.crossed_threshold().await?;
    assert_eq!(crossed.map(|usage| usage.entries), Some(1001));
    paths.push(spool.spool("b", now + Duration::from_millis(1001)).await?);
    assert_eq!(spool.crossed_threshold().await?, None);
    assert!(
        paths.iter().all(|path| path.is_file()),
        "a batch was dropped"
    );
    for path in paths.drain(..2) {
        std::fs::remove_file(path)?;
    }
    assert_eq!(spool.crossed_threshold().await?, None);
    spool.spool("b", now + Duration::from_millis(1002)).await?;
    spool.spool("b", now + Duration::from_millis(1003)).await?;
    assert_eq!(
        spool.crossed_threshold().await?.map(|usage| usage.entries),
        Some(1002)
    );
    Ok(())
}

#[tokio::test]
async fn a_rejected_live_batch_is_dead_lettered() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let spool = CaptureSpool::new(dir.path().join("spool"));
    let dead = spool.dead_dir();
    let transport = RuleTransport::with(vec![("poison", Verdict::Reject)]);
    let writer = CaptureWriter::new(&transport, spool, None, None);
    let now = now_at("2026-10-10T12:00:00Z")?;
    let Err(error) = writer.write(&batch("poison", 1)?, now).await else {
        return Err("expected the rejected batch to fail the write".into());
    };
    assert!(error.to_string().contains("capture HTTP 422"));
    let dead_letters = bodies(&dead)?;
    let [letter] = dead_letters.as_slice() else {
        return Err("expected one dead letter".into());
    };
    assert!(letter.contains(r#""session_key":"poison""#));
    writer.write(&batch("healthy", 1)?, now).await?;
    assert_eq!(transport.posted().len(), 2);
    Ok(())
}

#[tokio::test]
async fn every_unsent_chunk_is_spooled_when_the_server_fails()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let spool = CaptureSpool::new(dir.path().join("spool"));
    let transport = RuleTransport::with(vec![(r#""event_id":"e1""#, Verdict::Unavailable)]);
    let writer = CaptureWriter::new(&transport, spool, NonZeroUsize::new(700), None);
    let now = now_at("2026-10-10T12:00:00Z")?;
    let Err(_) = writer.write(&batch("split", 3)?, now).await else {
        return Err("expected the failed chunk to fail the write".into());
    };
    assert_eq!(transport.posted().len(), 2);
    let spooled = bodies(&dir.path().join("spool"))?;
    assert_eq!(spooled.len(), 2);
    assert!(
        spooled
            .iter()
            .any(|body| body.contains(r#""event_id":"e1""#))
    );
    assert!(
        spooled
            .iter()
            .any(|body| body.contains(r#""event_id":"e2""#))
    );
    Ok(())
}
