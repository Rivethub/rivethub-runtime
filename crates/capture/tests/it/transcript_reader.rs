use jiff::Timestamp;
use std::str::FromStr;

use kernel::transcript::TranscriptReader;
use wire::message::Role;
use wire::session_id::SessionId;

const SESSION: &str = "33333333-3333-3333-3333-333333333333";

fn store() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("projects").join("-work-demo");
    std::fs::create_dir_all(&project)?;
    std::fs::copy(
        testkit::recording("capture/claude-hook/transcript.jsonl"),
        project.join(format!("{SESSION}.jsonl")),
    )?;
    Ok(dir)
}

fn at(text: &str) -> Result<Timestamp, jiff::Error> {
    text.parse()
}

fn reader(dir: &tempfile::TempDir) -> capture::transcript::ClaudeTranscriptReader {
    capture::transcript::ClaudeTranscriptReader::new(dir.path().to_path_buf())
}

#[tokio::test]
async fn sessions_list_the_recorded_store() -> Result<(), Box<dyn std::error::Error>> {
    let dir = store()?;
    let sessions = reader(&dir).sessions(dir.path()).await?;
    assert_eq!(
        sessions,
        vec![SessionId::from_str(&format!("claude-code:{SESSION}"))?]
    );
    Ok(())
}

#[tokio::test]
async fn read_maps_the_recorded_lines() -> Result<(), Box<dyn std::error::Error>> {
    let dir = store()?;
    let session = SessionId::from_str(&format!("claude-code:{SESSION}"))?;
    let turns = reader(&dir).read(&session).await?;
    let long = format!("Two files: {}😀", "z".repeat(16010));
    let expected = vec![
        ("user", "list the files".to_owned(), "2026-10-10T12:00:00Z"),
        (
            "assistant",
            "Running ls.".to_owned(),
            "2026-10-10T12:00:01Z",
        ),
        ("tool", "a.txt\nb.txt".to_owned(), "2026-10-10T12:00:02Z"),
        ("assistant", long, "2026-10-10T12:00:03Z"),
        ("assistant", "no uuid".to_owned(), "2026-10-10T12:00:04Z"),
    ];
    let mut failures = Vec::new();
    if turns.len() != expected.len() {
        failures.push(format!(
            "{} turn(s), expected {}",
            turns.len(),
            expected.len()
        ));
    }
    for (turn, (role, content, when)) in turns.iter().zip(&expected) {
        let role = match *role {
            "user" => Role::User,
            "assistant" => Role::Assistant,
            _ => Role::Tool,
        };
        if turn.role != role || turn.content != *content || turn.at != at(when)? {
            failures.push(format!(
                "turn mismatch: {:?} / {:?} / {}",
                turn.role, turn.content, turn.at
            ));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n").into())
    }
}

#[tokio::test]
async fn tail_returns_turns_after_the_timestamp() -> Result<(), Box<dyn std::error::Error>> {
    let dir = store()?;
    let session = SessionId::from_str(&format!("claude-code:{SESSION}"))?;
    let turns = reader(&dir)
        .tail(&session, at("2026-10-10T12:00:02Z")?)
        .await?;
    assert_eq!(turns.len(), 2);
    let [first, second] = turns.as_slice() else {
        return Err("expected exactly two turns".into());
    };
    assert_eq!(first.content, format!("Two files: {}😀", "z".repeat(16010)));
    assert_eq!(second.content, "no uuid");
    Ok(())
}

#[tokio::test]
async fn unknown_sessions_read_empty() -> Result<(), Box<dyn std::error::Error>> {
    let dir = store()?;
    let session = SessionId::from_str("claude-code:44444444-4444-4444-4444-444444444444")?;
    assert_eq!(reader(&dir).read(&session).await?.len(), 0);
    Ok(())
}

#[tokio::test]
async fn oversized_transcripts_cut_at_a_line_boundary() -> Result<(), Box<dyn std::error::Error>> {
    let dir = store()?;
    let project = dir.path().join("projects").join("-work-big");
    std::fs::create_dir_all(&project)?;
    let line = r#"{"type":"user","sessionId":"55555555-5555-5555-5555-555555555555","timestamp":"2026-10-10T12:00:00.000Z","message":{"role":"user","content":"hello"}}"#;
    let mut body = String::new();
    while body.len() < 9 * 1024 * 1024 {
        body.push_str(line);
        body.push('\n');
    }
    std::fs::write(
        project.join("55555555-5555-5555-5555-555555555555.jsonl"),
        body,
    )?;
    let session = SessionId::from_str("claude-code:55555555-5555-5555-5555-555555555555")?;
    let turns = reader(&dir).read(&session).await?;
    assert_ne!(turns.len(), 0);
    assert!(turns.iter().all(|turn| turn.content == "hello"));
    Ok(())
}

#[test]
fn unparseable_lines_are_reported_not_dropped_silently() {
    let text = concat!(
        r#"{"type":"user","timestamp":"2026-10-10T12:00:00.000Z","message":{"role":"user","content":"hello"}}"#,
        "\n{not json\n",
    );
    let parsed = capture::transcript::parse_lines(text);
    assert_eq!(parsed.lines.len(), 1);
    assert_eq!(parsed.skipped.len(), 1);
    assert_eq!(parsed.skipped.first().map(|line| line.number), Some(2));
}

#[tokio::test]
async fn lines_without_parseable_timestamps_are_not_turns() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("projects").join("-work-clock");
    std::fs::create_dir_all(&project)?;
    std::fs::write(
        project.join("66666666-6666-6666-6666-666666666666.jsonl"),
        concat!(
            r#"{"type":"user","timestamp":"2026-10-10T12:00:00.000Z","message":{"role":"user","content":"hello"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"not-a-timestamp","message":{"role":"assistant","content":"hi"}}"#,
            "\n",
        ),
    )?;
    let session = SessionId::from_str("claude-code:66666666-6666-6666-6666-666666666666")?;
    let turns = reader(&dir).read(&session).await?;
    assert_eq!(turns.len(), 1);
    assert_eq!(
        turns.first().map(|turn| turn.content.clone()),
        Some("hello".to_owned())
    );
    Ok(())
}

#[test]
fn unparseable_timestamps_are_reported_like_skipped_lines() {
    let text = concat!(
        r#"{"type":"user","timestamp":"2026-10-10T12:00:00.000Z","message":{"role":"user","content":"hello"}}"#,
        "\n\n",
        r#"{"type":"assistant","timestamp":"not-a-timestamp","message":{"role":"assistant","content":"hi"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":"untimed"}}"#,
        "\n",
    );
    let parsed = capture::transcript::parse_lines(text);
    assert_eq!(parsed.skipped, []);
    let turns = capture::transcript::turns_of(&parsed.lines);
    assert_eq!(turns.turns.len(), 1);
    assert_eq!(
        turns
            .skipped
            .iter()
            .map(|line| line.number)
            .collect::<Vec<_>>(),
        [3]
    );
    assert!(
        turns
            .skipped
            .first()
            .is_some_and(|line| line.reason.contains("not-a-timestamp"))
    );
}
