mod captured;
mod claude;
mod display;
mod grok;
mod tools;

use std::path::Path;
use std::path::PathBuf;

use kernel::transcript::StoreTranscript;
use kernel::transcript::ToolOutputRead;
use kernel::transcript::TranscriptRead;
use kernel::transcript::TranscriptReaderError;
use kernel::transcript::TranscriptWindow;
use wire::capture::CaptureMessage;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

use super::StoreRoots;
use crate::readers::StoreReader;
use crate::readers::blocking;
use crate::readers::codex::CodexTranscriptReader;
use crate::readers::opencode::OpencodeTranscriptReader;

pub(super) async fn transcript(
    roots: &StoreRoots,
    harness: HarnessId,
    native: &str,
    window: TranscriptWindow,
) -> Result<TranscriptRead, TranscriptReaderError> {
    if matches!(harness, HarnessId::Cowork | HarnessId::Dsh) {
        return Ok(TranscriptRead::NoSource);
    }
    let Some(root) = roots.single(harness).map(Path::to_path_buf) else {
        return Ok(TranscriptRead::Read(StoreTranscript::default()));
    };
    let native = native.to_owned();
    if harness == HarnessId::ClaudeCode {
        return blocking(move || claude::transcript(&root, &native, window))
            .await
            .map(TranscriptRead::Read);
    }
    if matches!(window, TranscriptWindow::Before(_)) {
        return Ok(TranscriptRead::Read(StoreTranscript::default()));
    }
    if harness == HarnessId::GrokBuild {
        return blocking(move || grok::transcript(&root, &native))
            .await
            .map(TranscriptRead::Read);
    }
    match captured_messages(harness, root, &native).await? {
        Some(messages) => captured::transcript(messages).map(TranscriptRead::Read),
        None => Ok(TranscriptRead::NoSource),
    }
}

pub(super) async fn tool_output(
    roots: &StoreRoots,
    harness: HarnessId,
    native: &str,
    call: &str,
) -> Result<ToolOutputRead, TranscriptReaderError> {
    if matches!(
        harness,
        HarnessId::GrokBuild | HarnessId::Dsh | HarnessId::Cowork
    ) {
        return Ok(ToolOutputRead::NoSource);
    }
    let Some(root) = roots.single(harness).map(Path::to_path_buf) else {
        return Ok(ToolOutputRead::Missing);
    };
    let native = native.to_owned();
    let call = call.to_owned();
    let found = match captured_messages(harness, root.clone(), &native).await? {
        Some(messages) => captured::tool_output(messages, &call),
        None => blocking(move || claude::tool_output(&root, &native, &call)).await,
    };
    found.map(|found| found.map_or(ToolOutputRead::Missing, ToolOutputRead::Found))
}

async fn captured_messages(
    harness: HarnessId,
    root: PathBuf,
    native: &str,
) -> Result<Option<Vec<CaptureMessage>>, TranscriptReaderError> {
    let session = SessionId::new(harness, native);
    let slice = match harness {
        HarnessId::Codex => CodexTranscriptReader::new(root).capture(&session).await?,
        HarnessId::Opencode => {
            OpencodeTranscriptReader::new(root)
                .capture(&session)
                .await?
        }
        HarnessId::Cursor | HarnessId::Hermes | HarnessId::KimiCode | HarnessId::Omp => {
            let Some(reader) = StoreReader::open(harness, root) else {
                return Ok(None);
            };
            reader.messages(&session, 0).await?
        }
        HarnessId::ClaudeCode | HarnessId::GrokBuild | HarnessId::Dsh | HarnessId::Cowork => {
            return Ok(None);
        }
    };
    Ok(Some(slice.messages))
}
