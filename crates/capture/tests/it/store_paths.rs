use std::os::unix::fs::symlink;
use std::path::Path;

use kernel::transcript::TranscriptReader;
use kernel::transcript::TranscriptReaderError;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

use capture::readers::StoreReader;

const SESSION: &str = "33333333-3333-3333-3333-333333333333";

fn reader(root: &Path) -> Result<StoreReader, Box<dyn std::error::Error>> {
    StoreReader::open(HarnessId::ClaudeCode, root.to_path_buf()).ok_or_else(|| "reader".into())
}

fn write_transcript(dir: &Path, native: &str) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(
        dir.join(format!("{native}.jsonl")),
        "{\"type\":\"user\",\"timestamp\":\"2026-10-14T12:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"inside\"}}\n",
    )?;
    Ok(())
}

#[tokio::test]
async fn a_transcript_inside_the_store_is_listed() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let real = root.path().join("projects").join("real");
    write_transcript(&real, SESSION)?;
    let linked = root.path().join("projects").join("linked");
    symlink(&real, &linked)?;
    let sessions = reader(root.path())?.sessions(root.path()).await?;
    let expected = SessionId::new(HarnessId::ClaudeCode, SESSION);
    if sessions != vec![expected.clone()] {
        return Err(format!("listed {}", sessions.len()).into());
    }
    let turns = reader(root.path())?.read(&expected).await?;
    if turns.first().map(|turn| turn.content.as_str()) != Some("inside") {
        return Err("read".into());
    }
    Ok(())
}

#[tokio::test]
async fn a_parent_segment_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    write_transcript(&root.path().join("projects").join("real"), SESSION)?;
    let session = SessionId::new(HarnessId::ClaudeCode, "..");
    let error = reader(root.path())?.read(&session).await.unwrap_err();
    assert!(matches!(error, TranscriptReaderError::Read { .. }));
    Ok(())
}

#[tokio::test]
async fn a_symlink_out_of_the_store_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let parent = tempfile::tempdir()?;
    let store = parent.path().join("store");
    let outside = parent.path().join("outside");
    write_transcript(&store.join("projects").join("real"), SESSION)?;
    write_transcript(&outside, "44444444-4444-4444-4444-444444444444")?;
    symlink(&outside, store.join("projects").join("escaped"))?;
    let error = reader(&store)?.sessions(&store).await.unwrap_err();
    let TranscriptReaderError::Read { message, .. } = error else {
        return Err("expected a read refusal".into());
    };
    assert!(message.contains("symlinks out of the store"));
    Ok(())
}

#[tokio::test]
async fn a_shared_prefix_sibling_is_not_part_of_the_store() -> Result<(), Box<dyn std::error::Error>>
{
    let parent = tempfile::tempdir()?;
    let store = parent.path().join("store");
    let sibling = parent.path().join("store-evil");
    write_transcript(&store.join("projects").join("real"), SESSION)?;
    write_transcript(
        &sibling.join("projects").join("real"),
        "44444444-4444-4444-4444-444444444444",
    )?;
    let sessions = reader(&store)?.sessions(&store).await?;
    assert_eq!(
        sessions,
        vec![SessionId::new(HarnessId::ClaudeCode, SESSION)]
    );
    Ok(())
}
